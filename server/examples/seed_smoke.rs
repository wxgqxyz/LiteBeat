//! T04 冒烟验证辅助：向指定 SQLite 库写入一个库根与给定媒体文件的曲目行。
//! 用法：cargo run --example seed_smoke -- <db_path> <root_dir> <file>...

use std::path::{Path, PathBuf};

use litebeat::db::{Db, DbOptions, Value};

fn mime_for(name: &str) -> &'static str {
    match name.rsplit('.').next().unwrap_or("") {
        "mp3" => "audio/mpeg",
        "m4a" => "audio/mp4",
        "wav" => "audio/wav",
        _ => "application/octet-stream",
    }
}

fn strip_verbatim(path: &Path) -> PathBuf {
    let text = path.display().to_string();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{rest}"))
    } else if let Some(rest) = text.strip_prefix(r"\\?\") {
        PathBuf::from(rest)
    } else {
        path.to_path_buf()
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    let db_path = PathBuf::from(args.next().ok_or("缺少 db 路径")?);
    let root = PathBuf::from(args.next().ok_or("缺少库根路径")?);
    let files: Vec<PathBuf> = args.map(PathBuf::from).collect();
    let db = Db::open(db_path, DbOptions::default())?;

    let plain = strip_verbatim(&std::fs::canonicalize(&root)?);
    let root_text = plain.display().to_string();
    let existing = db
        .query_rows("SELECT id FROM library_roots WHERE name = 'smoke'", vec![])
        .await?;
    let root_id = match existing.first() {
        Some(row) => row[0].as_i64().ok_or("库根缺少 id")?,
        None => {
            db.execute(
                "INSERT INTO library_roots(name, canonical_path) VALUES ('smoke', ?)",
                vec![Value::Text(root_text)],
            )
            .await?
            .last_rowid
        }
    };

    for file in &files {
        let name = file
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or("非法文件名")?
            .to_string();
        let metadata = std::fs::metadata(file)?;
        let mtime = metadata
            .modified()?
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos() as i64;
        let id = db
            .execute(
                "INSERT INTO tracks(root_id, relative_path, title, size_bytes, mtime_ns,
                                    sort_title, sort_artist, sort_album, mime)
                 VALUES (?, ?, ?, ?, ?, ?, '', '', ?)",
                vec![
                    Value::Integer(root_id),
                    Value::Text(name.clone()),
                    Value::Text(name.clone()),
                    Value::Integer(metadata.len() as i64),
                    Value::Integer(mtime),
                    Value::Text(name.clone()),
                    Value::Text(mime_for(&name).into()),
                ],
            )
            .await?
            .last_rowid;
        println!("track {id}: {name}");
    }
    db.close();
    Ok(())
}
