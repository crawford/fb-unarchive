use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use log::{debug, info, trace, LevelFilter};
use std::fs::{self, File};
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
