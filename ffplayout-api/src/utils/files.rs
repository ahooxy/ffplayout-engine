use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use actix_multipart::Multipart;
use actix_web::{web, HttpResponse};
use futures_util::TryStreamExt as _;
use lexical_sort::{natural_lexical_cmp, PathSort};
use rand::{distributions::Alphanumeric, Rng};
use relative_path::RelativePath;
use serde::{Deserialize, Serialize};
use sqlx::{Pool, Sqlite};

use simplelog::*;

use crate::utils::{errors::ServiceError, playout_config};
use ffplayout_lib::utils::{file_extension, MediaProbe};

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct PathObject {
    pub source: String,
    parent: Option<String>,
    folders: Option<Vec<String>>,
    files: Option<Vec<VideoFile>>,
    #[serde(default)]
    pub folders_only: bool,
}

impl PathObject {
    fn new(source: String, parent: Option<String>) -> Self {
        Self {
            source,
            parent,
            folders: Some(vec![]),
            files: Some(vec![]),
            folders_only: false,
        }
    }
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct MoveObject {
    source: String,
    target: String,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct VideoFile {
    name: String,
    duration: f64,
}

/// Normalize absolut path
///
/// This function takes care, that it is not possible to break out from root_path.
/// It also gives always a relative path back.
pub fn norm_abs_path(root_path: &Path, input_path: &str) -> (PathBuf, String, String) {
    let path_relative = RelativePath::new(&root_path.to_string_lossy())
        .normalize()
        .to_string()
        .replace("../", "");
    let mut source_relative = RelativePath::new(input_path)
        .normalize()
        .to_string()
        .replace("../", "");
    let path_suffix = root_path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    if input_path.starts_with(&*root_path.to_string_lossy())
        || source_relative.starts_with(&path_relative)
    {
        source_relative = source_relative
            .strip_prefix(&path_relative)
            .and_then(|s| s.strip_prefix('/'))
            .unwrap_or_default()
            .to_string();
    } else {
        source_relative = source_relative
            .strip_prefix(&path_suffix)
            .and_then(|s| s.strip_prefix('/'))
            .unwrap_or(&source_relative)
            .to_string();
    }

    let path = &root_path.join(&source_relative);

    (path.to_path_buf(), path_suffix, source_relative)
}

/// File Browser
///
/// Take input path and give file and folder list from it back.
/// Input should be a relative path segment, but when it is a absolut path, the norm_abs_path function
/// will take care, that user can not break out from given storage path in config.
pub async fn browser(
    conn: &Pool<Sqlite>,
    id: i32,
    path_obj: &PathObject,
) -> Result<PathObject, ServiceError> {
    let (config, channel) = playout_config(conn, &id).await?;
    let mut channel_extensions = channel
        .extra_extensions
        .split(',')
        .map(|e| e.to_string())
        .collect::<Vec<String>>();
    let mut extensions = config.storage.extensions;
    extensions.append(&mut channel_extensions);

    let (path, parent, path_component) = norm_abs_path(&config.storage.path, &path_obj.source);
    let mut obj = PathObject::new(path_component, Some(parent));
    obj.folders_only = path_obj.folders_only;

    let mut paths: Vec<PathBuf> = match fs::read_dir(path) {
        Ok(p) => p.filter_map(|r| r.ok()).map(|p| p.path()).collect(),
        Err(e) => {
            error!("{e} in {}", path_obj.source);
            return Err(ServiceError::NoContent(e.to_string()));
        }
    };

    paths.path_sort(natural_lexical_cmp);
    let mut files = vec![];
    let mut folders = vec![];

    for path in paths {
        // ignore hidden files/folders on unix
        if path.display().to_string().contains("/.") {
            continue;
        }

        if path.is_dir() {
            folders.push(path.file_name().unwrap().to_string_lossy().to_string());
        } else if path.is_file() && !path_obj.folders_only {
            if let Some(ext) = file_extension(&path) {
                if extensions.contains(&ext.to_string().to_lowercase()) {
                    match MediaProbe::new(&path.display().to_string()) {
                        Ok(probe) => {
                            let mut duration = 0.0;

                            if let Some(dur) = probe.format.duration {
                                duration = dur.parse().unwrap_or_default()
                            }

                            let video = VideoFile {
                                name: path.file_name().unwrap().to_string_lossy().to_string(),
                                duration,
                            };
                            files.push(video);
                        }
                        Err(e) => error!("{e:?}"),
                    };
                }
            }
        }
    }

    obj.folders = Some(folders);
    obj.files = Some(files);

    Ok(obj)
}

pub async fn create_directory(
    conn: &Pool<Sqlite>,
    id: i32,
    path_obj: &PathObject,
) -> Result<HttpResponse, ServiceError> {
    let (config, _) = playout_config(conn, &id).await?;
    let (path, _, _) = norm_abs_path(&config.storage.path, &path_obj.source);

    if let Err(e) = fs::create_dir_all(&path) {
        return Err(ServiceError::BadRequest(e.to_string()));
    }

    info!("create folder: <b><magenta>{}</></b>", path.display());

    Ok(HttpResponse::Ok().into())
}

// fn copy_and_delete(source: &PathBuf, target: &PathBuf) -> Result<PathObject, ServiceError> {
//     match fs::copy(&source, &target) {
//         Ok(_) => {
//             if let Err(e) = fs::remove_file(source) {
//                 error!("{e}");
//                 return Err(ServiceError::BadRequest(
//                     "Removing File not possible!".into(),
//                 ));
//             };

//             return Ok(PathObject::new(target.display().to_string()));
//         }
//         Err(e) => {
//             error!("{e}");
//             Err(ServiceError::BadRequest("Error in file copy!".into()))
//         }
//     }
// }

fn rename(source: &PathBuf, target: &PathBuf) -> Result<MoveObject, ServiceError> {
    match fs::rename(source, target) {
        Ok(_) => Ok(MoveObject {
            source: source
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string(),
            target: target
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string(),
        }),
        Err(e) => {
            error!("{e}");
            Err(ServiceError::BadRequest("Rename failed!".into()))
        }
    }
}

pub async fn rename_file(
    conn: &Pool<Sqlite>,
    id: i32,
    move_object: &MoveObject,
) -> Result<MoveObject, ServiceError> {
    let (config, _) = playout_config(conn, &id).await?;
    let (source_path, _, _) = norm_abs_path(&config.storage.path, &move_object.source);
    let (mut target_path, _, _) = norm_abs_path(&config.storage.path, &move_object.target);

    if !source_path.exists() {
        return Err(ServiceError::BadRequest("Source file not exist!".into()));
    }

    if (source_path.is_dir() || source_path.is_file()) && source_path.parent() == Some(&target_path)
    {
        return rename(&source_path, &target_path);
    }

    if target_path.is_dir() {
        target_path = target_path.join(source_path.file_name().unwrap());
    }

    if target_path.is_file() {
        return Err(ServiceError::BadRequest(
            "Target file already exists!".into(),
        ));
    }

    if source_path.is_file() && target_path.parent().is_some() {
        return rename(&source_path, &target_path);
    }

    Err(ServiceError::InternalServerError)
}

pub async fn remove_file_or_folder(
    conn: &Pool<Sqlite>,
    id: i32,
    source_path: &str,
) -> Result<(), ServiceError> {
    let (config, _) = playout_config(conn, &id).await?;
    let (source, _, _) = norm_abs_path(&config.storage.path, source_path);

    if !source.exists() {
        return Err(ServiceError::BadRequest("Source does not exists!".into()));
    }

    if source.is_dir() {
        match fs::remove_dir(source) {
            Ok(_) => return Ok(()),
            Err(e) => {
                error!("{e}");
                return Err(ServiceError::BadRequest(
                    "Delete folder failed! (Folder must be empty)".into(),
                ));
            }
        };
    }

    if source.is_file() {
        match fs::remove_file(source) {
            Ok(_) => return Ok(()),
            Err(e) => {
                error!("{e}");
                return Err(ServiceError::BadRequest("Delete file failed!".into()));
            }
        };
    }

    Err(ServiceError::InternalServerError)
}

async fn valid_path(conn: &Pool<Sqlite>, id: i32, path: &str) -> Result<PathBuf, ServiceError> {
    let (config, _) = playout_config(conn, &id).await?;
    let (test_path, _, _) = norm_abs_path(&config.storage.path, path);

    if !test_path.is_dir() {
        return Err(ServiceError::BadRequest("Target folder not exists!".into()));
    }

    Ok(test_path)
}

pub async fn upload(
    conn: &Pool<Sqlite>,
    id: i32,
    _size: u64,
    mut payload: Multipart,
    path: &Path,
    abs_path: bool,
) -> Result<HttpResponse, ServiceError> {
    while let Some(mut field) = payload.try_next().await? {
        let content_disposition = field.content_disposition();
        debug!("{content_disposition}");
        let rand_string: String = rand::thread_rng()
            .sample_iter(&Alphanumeric)
            .take(20)
            .map(char::from)
            .collect();
        let filename = content_disposition
            .get_filename()
            .map_or_else(|| rand_string.to_string(), sanitize_filename::sanitize);

        let filepath = if abs_path {
            path.to_path_buf()
        } else {
            valid_path(conn, id, &path.to_string_lossy())
                .await?
                .join(filename)
        };
        let filepath_clone = filepath.clone();

        let _file_size = match filepath.metadata() {
            Ok(metadata) => metadata.len(),
            Err(_) => 0,
        };

        // INFO: File exist check should be enough because file size and content length are different.
        // The error catching in the loop should normally prevent unfinished files from existing on disk.
        // If this is not enough, a second check can be implemented: is_close(file_size as i64, size as i64, 1000)
        if filepath.is_file() {
            return Err(ServiceError::Conflict("Target already exists!".into()));
        }

        let mut f = web::block(|| std::fs::File::create(filepath_clone)).await??;

        loop {
            match field.try_next().await {
                Ok(Some(chunk)) => {
                    f = web::block(move || f.write_all(&chunk).map(|_| f)).await??;
                }

                Ok(None) => break,

                Err(e) => {
                    if e.to_string().contains("stream is incomplete") {
                        info!("Delete non finished file: {filepath:?}");

                        tokio::fs::remove_file(filepath).await?
                    }

                    return Err(e.into());
                }
            }
        }
    }

    Ok(HttpResponse::Ok().into())
}
