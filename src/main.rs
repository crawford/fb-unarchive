// Copyright 2020 Alex Crawford
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use anyhow::{anyhow, Context, Result};
use chrono::naive::NaiveDateTime;
use imagemeta::exif;
use img_parts::{jpeg::Jpeg, ImageEXIF};
use log::{debug, info, trace, warn, LevelFilter};
use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Cursor};
use std::path::{Path, PathBuf};
use structopt::StructOpt;

#[derive(Debug, StructOpt)]
#[structopt(
    about = "Transform and organize photos from a Facebook data dump (archive) according to the associated metadata"
)]
struct Options {
    #[structopt(short, long)]
    dry_run: bool,

    #[structopt(short, long, default_value = ".", parse(from_os_str))]
    input: PathBuf,

    #[structopt(short, long, default_value = "./out", parse(from_os_str))]
    output: PathBuf,

    #[structopt(long)]
    skip_photos: bool,

    #[structopt(long)]
    skip_videos: bool,

    #[structopt(short, long, parse(from_occurrences))]
    verbosity: u8,
}

#[derive(serde::Deserialize, Debug)]
struct Album {
    name: String,
    description: Option<String>,
    #[serde(default = "Vec::new", rename = "photos")]
    items: Vec<Item>,
}

#[derive(serde::Deserialize, Debug)]
struct Item {
    #[serde(
        with = "chrono::naive::serde::ts_seconds",
        rename = "creation_timestamp"
    )]
    timestamp: NaiveDateTime,
    #[serde(rename = "uri")]
    path: PathBuf,
    description: Option<String>,
    #[serde(default = "Vec::new")]
    comments: Vec<Comment>,
}

#[derive(serde::Deserialize, Debug)]
struct Comment {
    #[serde(with = "chrono::naive::serde::ts_seconds")]
    timestamp: NaiveDateTime,
    comment: Option<String>,
    author: String,
}

fn main() -> Result<()> {
    let mut opts = Options::from_args();

    env_logger::Builder::from_default_env()
        .filter_level(match opts.verbosity {
            0 => LevelFilter::Warn,
            1 => LevelFilter::Info,
            2 => LevelFilter::Debug,
            _ => LevelFilter::Trace,
        })
        .format_timestamp(None)
        .init();

    info!(
        "{} version: {}",
        structopt::clap::crate_name!(),
        structopt::clap::crate_version!()
    );

    if !opts.skip_videos {
        info!("Processing videos isn't implemented yet; skipping them");
        opts.skip_videos = true;
    }

    let albums = read_albums(&opts.input)?;
    trace!("Albums: {:#?}", albums);

    process_albums(&opts, albums)?;

    Ok(())
}

fn read_albums(root: &Path) -> Result<Vec<Album>> {
    debug!("Finding albums");

    let mut albums = Vec::new();
    let dir = root.join("photos_and_videos").join("album");
    for entry in fs::read_dir(&dir).context(format!("Unable to list albums ({})", dir.display()))? {
        let path = entry?.path();
        if path.extension().and_then(|x| x.to_str()) != Some("json") {
            trace!("Skipping {}", path.display());
            continue;
        }

        trace!("Adding {}", path.display());
        let mut album: Album = serde_json::from_reader(File::open(path)?)?;
        for item in album.items.iter_mut() {
            item.path = root.join(&item.path);
        }
        albums.push(album);
    }

    Ok(albums)
}

fn process_albums<A: IntoIterator<Item = Album>>(opts: &Options, albums: A) -> Result<()> {
    debug!("Processing albums");

    for album in albums {
        let album_dir = opts.output.join(album.name);
        if !opts.dry_run {
            fs::create_dir_all(&album_dir)?;
        }

        for item in album.items {
            match item.path.extension().and_then(|x| x.to_str()) {
                Some("jpg") => process_jpeg(&item, &album_dir, opts)?,
                Some("mp4") => process_mp4(&item, &album_dir, opts)?,
                Some(ext) => {
                    warn!(
                        r#"Unrecognized file extension "{}"; skipping {}"#,
                        ext,
                        item.path.display()
                    );
                    continue;
                }
                None => {
                    warn!(r"Missing file extension; skipping {}", item.path.display());
                    continue;
                }
            }
        }
    }

    Ok(())
}

fn process_jpeg(item: &Item, dir: &Path, opts: &Options) -> Result<()> {
    if opts.skip_photos {
        trace!("Skipping photo {}", item.path.display());
        return Ok(());
    }

    let mut jpeg = Jpeg::read(&mut BufReader::new(
        File::open(&item.path).context(format!("Failed to open {}", item.path.display()))?,
    ))
    .map_err(|e| anyhow!("Failed to parse {}: {}", item.path.display(), e))?;

    let description = item.description.clone().into_iter();
    let comments = item.comments.iter().filter_map(|c| {
        c.comment.as_ref().map(|comment| {
            format!(
                r#""{}" -{} ({})"#,
                comment,
                c.author,
                c.timestamp.format("%F %r")
            )
        })
    });
    let combined = description.chain(comments).collect::<Vec<_>>().join("\n");

    let exif = exif::Exif {
        ifds: vec![exif::Ifd {
            id: 0,
            entries: vec![
                exif::Entry {
                    tag: rexif::ExifTag::ImageDescription as u16,
                    data: exif::EntryData::Ascii(combined),
                },
                exif::Entry {
                    tag: rexif::ExifTag::DateTime as u16,
                    data: exif::EntryData::Ascii(
                        item.timestamp.format("%Y:%m:%d %H:%M:%S").to_string(),
                    ),
                },
            ],
            children: Vec::new(),
        }],
    };

    trace!("Writing metadata for {}: {:#?}", item.path.display(), exif);
    let mut raw_exif = Cursor::new(Vec::new());
    exif.encode(&mut raw_exif).map_err(|e| anyhow!("{}", e))?;
    jpeg.set_exif(Some(raw_exif.into_inner()));

    let out_path = dir.join(
        item.path
            .file_name()
            .ok_or_else(|| anyhow!("missing filename"))?,
    );
    if !opts.dry_run {
        trace!("Outputting {}", out_path.display());
        jpeg.write_to(&mut BufWriter::new(File::create(out_path)?))?;
    }

    Ok(())
}

fn process_mp4(item: &Item, _dir: &Path, opts: &Options) -> Result<()> {
    if opts.skip_videos {
        trace!("Skipping video {}", item.path.display());
        return Ok(());
    }

    Ok(())
}
