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
use chrono::{naive::NaiveDateTime, offset::Utc, DateTime};
use imagemeta::exif;
use img_parts::{jpeg::Jpeg, ImageEXIF};
use log::{debug, info, trace, warn, LevelFilter};
use serde::Deserialize;
use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Cursor};
use std::path::{Path, PathBuf};
use std::time::SystemTime;
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

#[derive(Deserialize, Debug)]
struct Album {
    name: String,
    description: Option<String>,
    #[serde(default = "Vec::new", rename = "photos")]
    items: Vec<Item>,
}

#[derive(Deserialize, Debug)]
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

#[derive(Deserialize, Debug)]
struct Comment {
    #[serde(with = "chrono::naive::serde::ts_seconds")]
    timestamp: NaiveDateTime,
    comment: Option<String>,
    author: String,
}

fn main() -> Result<()> {
    let opts = Options::from_args();

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

    let albums = read_albums(&opts.input).context("read_albums")?;
    trace!("Albums: {:#?}", albums);
    process_albums(&opts, albums).context("process_albums")?;

    let videos = read_videos(&opts.input).context("read_videos")?;
    trace!("Videos: {:#?}", videos);
    process_videos(&opts, videos).context("process_videos")?;

    Ok(())
}

fn read_albums(root: &Path) -> Result<Vec<Album>> {
    debug!("Finding albums");

    let mut albums = Vec::new();
    let dir = root.join("photos_and_videos").join("album");
    for entry in fs::read_dir(&dir).context(format!("list directory {}", dir.display()))? {
        let path = entry.context("entry")?.path();
        if path.extension().and_then(|x| x.to_str()) != Some("json") {
            trace!("Skipping {}", path.display());
            continue;
        }

        trace!("Adding {}", path.display());
        let mut album: Album = serde_json::from_reader(&mut BufReader::new(
            File::open(&path).context(format!("open {}", path.display()))?,
        ))
        .context("parse json")?;
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
            fs::create_dir_all(&album_dir)
                .context(format!("create directory {}", &album_dir.display()))?;
        }

        for item in album.items {
            process_item(&item, &album_dir, opts).context("process item")?;
        }
    }

    Ok(())
}

fn process_item(item: &Item, out_dir: &Path, opts: &Options) -> Result<()> {
    match item.path.extension().and_then(|x| x.to_str()) {
        Some("jpg") => process_jpeg(&item, out_dir, opts).context("process jpeg")?,
        Some("mp4") => process_video(&item, out_dir, opts).context("process video")?,
        Some("flv") => process_video(&item, out_dir, opts).context("process video")?,
        Some(ext) => {
            warn!(
                r#"Unrecognized file extension "{}"; skipping {}"#,
                ext,
                item.path.display()
            );
            return Ok(());
        }
        None => {
            warn!(r"Missing file extension; skipping {}", item.path.display());
            return Ok(());
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
        File::open(&item.path).context(format!("open {}", item.path.display()))?,
    ))
    .map_err(|e| anyhow!("Failed to parse {}: {}", item.path.display(), e))
    .context("parse jpeg")?;

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
                    tag: rexif::ExifTag::UserComment as u16,
                    data: exif::EntryData::Ascii(combined),
                },
                exif::Entry {
                    tag: rexif::ExifTag::DateTimeOriginal as u16,
                    data: exif::EntryData::Ascii(
                        item.timestamp.format("%Y:%m:%d %H:%M:%S").to_string(),
                    ),
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
    exif.encode(&mut raw_exif).context("exif encode")?;
    jpeg.set_exif(Some(raw_exif.into_inner()));

    let out_path = dir.join(item.path.file_name().context("file name")?);
    if !opts.dry_run {
        trace!("Outputting {}", out_path.display());
        jpeg.write_to(&mut BufWriter::new(
            File::create(&out_path).context("create")?,
        ))
        .context(format!("write file {}", out_path.display()))?;
    }

    Ok(())
}

fn process_video(item: &Item, dir: &Path, opts: &Options) -> Result<()> {
    if opts.skip_videos {
        trace!("Skipping video {}", item.path.display());
        return Ok(());
    }

    let in_path = opts.input.join(&item.path);
    let out_path = dir.join(item.path.file_name().context("file name")?);
    let timestamp = Into::<SystemTime>::into(DateTime::<Utc>::from_utc(item.timestamp, Utc)).into();

    fs::copy(&in_path, &out_path).context(format!(
        "copy {} to {}",
        in_path.display(),
        out_path.display()
    ))?;
    filetime::set_file_handle_times(
        &File::open(&out_path).context("open")?,
        Some(timestamp),
        Some(timestamp),
    )
    .context(format!("set times on {}", out_path.display()))?;

    Ok(())
}

fn read_videos(root: &Path) -> Result<Vec<Item>> {
    let path = root.join("photos_and_videos").join("your_videos.json");
    let videos =
        &mut BufReader::new(File::open(&path).context(format!("open {}", path.display()))?);

    Ok(Vec::<Item>::deserialize(
        serde_json::from_reader::<_, serde_json::Value>(videos)
            .context("parse json (videos)")?
            .get("videos")
            .context("videos")?,
    )
    .context("parse json")?)
}

fn process_videos<V: IntoIterator<Item = Item>>(opts: &Options, videos: V) -> Result<()> {
    debug!("Processing videos");

    let out_path = opts.output.join("videos");
    if !opts.dry_run {
        fs::create_dir_all(&out_path)
            .context(format!("create directory {}", out_path.display()))?;
    }

    for video in videos {
        process_item(&video, &out_path, opts).context("process item")?;
    }

    Ok(())
}
