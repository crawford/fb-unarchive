use anyhow::{anyhow, Context, Result};
use chrono::naive::NaiveDateTime;
use imagemeta::exif;
use img_parts::{jpeg::Jpeg, ImageEXIF};
use log::{debug, info, trace, LevelFilter};
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

    #[structopt(short, long, default_value = "./photos_and_videos", parse(from_os_str))]
    input: PathBuf,

    #[structopt(short, long, default_value = "./out", parse(from_os_str))]
    output: PathBuf,

    #[structopt(short, long, parse(from_occurrences))]
    verbosity: u8,
}

#[derive(serde::Deserialize, Debug)]
struct Album {
    name: String,
    description: Option<String>,
    #[serde(default = "Vec::new")]
    photos: Vec<Photo>,
}

#[derive(serde::Deserialize, Debug)]
struct Photo {
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

    let albums = read_albums(&opts.input.join("album"))?;
    trace!("Albums: {:#?}", albums);

    process_albums(albums, &opts.output)?;

    Ok(())
}

fn read_albums(dir: &Path) -> Result<Vec<Album>> {
    debug!("Finding albums");

    let mut albums = Vec::new();
    for entry in fs::read_dir(dir).context(format!("Unable to list albums ({})", dir.display()))? {
        let path = entry?.path();
        if path.extension().and_then(|x| x.to_str()) != Some("json") {
            trace!("Skipping {}", path.display());
            continue;
        }

        trace!("Adding {}", path.display());
        albums.push(serde_json::from_reader(File::open(path)?)?);
    }

    Ok(albums)
}

fn process_albums<A: IntoIterator<Item = Album>>(albums: A, out_dir: &Path) -> Result<()> {
    debug!("Processing albums");

    for album in albums {
        let album_dir = out_dir.join(album.name);
        fs::create_dir_all(&album_dir)?;

        for photo in album.photos {
            let description = photo.description.into_iter();
            let comments = photo.comments.into_iter().filter_map(|c| {
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

            let mut jpeg = Jpeg::read(&mut BufReader::new(File::open(&photo.path)?))
                .map_err(|e| anyhow!("{}", e))?;

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
                                photo.timestamp.format("%Y:%m:%d %H:%M:%S").to_string(),
                            ),
                        },
                    ],
                    children: Vec::new(),
                }],
            };

            trace!("Writing metadata for {}: {:#?}", photo.path.display(), exif);
            let mut raw_exif = Cursor::new(Vec::new());
            exif.encode(&mut raw_exif).map_err(|e| anyhow!("{}", e))?;
            jpeg.set_exif(Some(raw_exif.into_inner()));

            let out_path =
                album_dir.join(photo.path.file_name().ok_or(anyhow!("missing filename"))?);
            trace!("Outputting {}", out_path.display());
            jpeg.write_to(&mut BufWriter::new(File::create(out_path)?))?;
        }
    }

    Ok(())
}
