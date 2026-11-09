#![cfg(target_os = "linux")]

use std::{
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::symlink,
    time::Duration,
};

use arcdos_transfers::{MAX_CHUNK_BYTES, TransferError, UploadSpec, UploadTarget, Uploads};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

fn spec(content: &[u8]) -> UploadSpec {
    UploadSpec {
        target: UploadTarget {
            parent_id: "parent".to_owned(),
            name: "résumé.pdf".to_owned(),
            parent_revision: "revision".to_owned(),
        },
        length: content.len() as u64,
        sha256: Sha256::digest(content).into(),
    }
}

#[tokio::test]
async fn resumes_at_the_manifest_offset_after_restart() {
    let temp = tempfile::tempdir().unwrap();
    let directory = temp.path().join("transfers");
    let store = Uploads::open(&directory).unwrap();
    let upload = store.create(spec(b"abcdef")).await.unwrap();
    assert_eq!(
        store
            .append(upload.id, 0, 3, &b"abc"[..])
            .await
            .unwrap()
            .offset,
        3
    );

    let stage = directory.join(format!("{}.upload", upload.id));
    OpenOptions::new()
        .append(true)
        .open(&stage)
        .unwrap()
        .write_all(b"uncommitted")
        .unwrap();
    drop(store);

    let store = Uploads::open(&directory).unwrap();
    assert_eq!(store.status(upload.id).await.unwrap().offset, 3);
    assert!(matches!(
        store.append(upload.id, 0, 3, &b"abc"[..]).await,
        Err(TransferError::Offset { expected: 3 })
    ));
    let resumed = store.append(upload.id, 3, 3, &b"def"[..]).await.unwrap();
    assert_eq!(resumed.offset, 6);
    assert_eq!(resumed.target, upload.target);
    assert_eq!(fs::read(stage).unwrap(), b"abcdef");
    assert!(store.prepare_commit(upload.id).await.unwrap().is_staged());
}

#[tokio::test]
async fn validates_exact_chunk_length_and_keeps_committed_bytes() {
    let temp = tempfile::tempdir().unwrap();
    let directory = temp.path().join("transfers");
    let store = Uploads::open(&directory).unwrap();
    let upload = store.create(spec(b"abcdef")).await.unwrap();
    store.append(upload.id, 0, 3, &b"abc"[..]).await.unwrap();

    for (length, body) in [
        (0, &b""[..]),
        (MAX_CHUNK_BYTES + 1, &b""[..]),
        (4, &b"defg"[..]),
        (3, &b"de"[..]),
        (3, &b"defg"[..]),
    ] {
        assert!(matches!(
            store.append(upload.id, 3, length, body).await,
            Err(TransferError::InvalidLength)
        ));
        assert_eq!(store.status(upload.id).await.unwrap().offset, 3);
        assert_eq!(
            fs::read(directory.join(format!("{}.upload", upload.id))).unwrap(),
            b"abc"
        );
    }
}

#[tokio::test]
async fn refuses_to_publish_before_a_successful_rename_and_recovers_after_one() {
    let temp = tempfile::tempdir().unwrap();
    let directory = temp.path().join("transfers");
    let store = Uploads::open(&directory).unwrap();
    let upload = store.create(spec(b"contents")).await.unwrap();
    assert!(matches!(
        store.prepare_commit(upload.id).await,
        Err(TransferError::Incomplete)
    ));
    store
        .append(upload.id, 0, 8, &b"contents"[..])
        .await
        .unwrap();
    let prepared = store.prepare_commit(upload.id).await.unwrap();
    assert_eq!(prepared.upload().target, upload.target);
    assert!(prepared.is_staged());
    assert!(matches!(
        store.cancel(upload.id).await,
        Err(TransferError::Busy)
    ));
    assert!(matches!(
        prepared.finish().await,
        Err(TransferError::Conflict)
    ));

    let prepared = store.prepare_commit(upload.id).await.unwrap();
    let destination = temp.path().join("destination");
    let stage = directory.join(prepared.staging_name());
    assert!(prepared.identity().matches(&fs::metadata(&stage).unwrap()));
    fs::rename(stage, &destination).unwrap();
    drop(prepared);
    drop(store);

    let store = Uploads::open(&directory).unwrap();
    assert!(matches!(
        store.cancel(upload.id).await,
        Err(TransferError::Conflict)
    ));
    let recovered = store.prepare_commit(upload.id).await.unwrap();
    assert!(!recovered.is_staged());
    assert!(!recovered.upload().published);
    assert!(
        recovered
            .identity()
            .matches(&fs::metadata(&destination).unwrap())
    );
    recovered.finish().await.unwrap();
    assert!(store.status(upload.id).await.unwrap().published);
    let retry = store.prepare_commit(upload.id).await.unwrap();
    assert!(!retry.is_staged());
    assert!(retry.upload().published);
    retry.finish().await.unwrap();
    assert_eq!(fs::read(destination).unwrap(), b"contents");
}

#[tokio::test]
async fn rejects_a_changed_stage_and_allows_cancellation() {
    let temp = tempfile::tempdir().unwrap();
    let directory = temp.path().join("transfers");
    let store = Uploads::open(&directory).unwrap();
    let upload = store.create(spec(b"good")).await.unwrap();
    store.append(upload.id, 0, 4, &b"good"[..]).await.unwrap();
    let prepared = store.prepare_commit(upload.id).await.unwrap();
    let stage = directory.join(prepared.staging_name());
    drop(prepared);
    fs::write(&stage, b"evil").unwrap();

    assert!(matches!(
        store.prepare_commit(upload.id).await,
        Err(TransferError::Checksum)
    ));
    store.cancel(upload.id).await.unwrap();
    assert!(!stage.exists());
    assert!(matches!(
        store.status(upload.id).await,
        Err(TransferError::NotFound)
    ));
}

#[tokio::test]
async fn failed_manifest_update_keeps_the_previous_committed_offset() {
    let temp = tempfile::tempdir().unwrap();
    let directory = temp.path().join("transfers");
    let store = Uploads::open(&directory).unwrap();
    let upload = store.create(spec(b"abc")).await.unwrap();
    let blocked_manifest = directory.join(format!("{}.tmp", upload.id));
    fs::create_dir(&blocked_manifest).unwrap();

    assert!(matches!(
        store.append(upload.id, 0, 3, &b"bad"[..]).await,
        Err(TransferError::Io(_))
    ));
    assert_eq!(store.status(upload.id).await.unwrap().offset, 0);

    fs::remove_dir(blocked_manifest).unwrap();
    store.append(upload.id, 0, 3, &b"abc"[..]).await.unwrap();
    assert!(store.prepare_commit(upload.id).await.unwrap().is_staged());
}

#[tokio::test]
async fn aborted_caller_does_not_release_an_in_flight_writer() {
    let temp = tempfile::tempdir().unwrap();
    let store = Uploads::open(&temp.path().join("transfers")).unwrap();
    let upload = store.create(spec(b"abc")).await.unwrap();
    let (mut sender, body) = tokio::io::duplex(1);
    let request = {
        let store = store.clone();
        tokio::spawn(async move { store.append(upload.id, 0, 3, body).await })
    };
    // The one-byte pipe requires the worker to read before this completes.
    tokio::time::timeout(Duration::from_secs(3), sender.write_all(b"ab"))
        .await
        .unwrap()
        .unwrap();
    request.abort();
    assert!(request.await.unwrap_err().is_cancelled());
    assert!(matches!(
        store.cancel(upload.id).await,
        Err(TransferError::Busy)
    ));
    drop(sender);

    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match store.append(upload.id, 0, 3, &b"abc"[..]).await {
                Ok(upload) => break assert_eq!(upload.offset, 3),
                Err(TransferError::Busy) => tokio::time::sleep(Duration::from_millis(5)).await,
                Err(error) => panic!("unexpected upload failure: {error}"),
            }
        }
    })
    .await
    .unwrap();
    assert!(store.prepare_commit(upload.id).await.unwrap().is_staged());
}

#[tokio::test]
async fn refuses_other_processes_and_staging_symlinks() {
    let temp = tempfile::tempdir().unwrap();
    let directory = temp.path().join("transfers");
    let store = Uploads::open(&directory).unwrap();
    assert!(matches!(
        Uploads::open(&directory),
        Err(TransferError::Busy)
    ));
    let upload = store.create(spec(b"abc")).await.unwrap();
    let stage = directory.join(format!("{}.upload", upload.id));
    let outside = temp.path().join("outside");
    fs::write(&outside, b"secret").unwrap();
    fs::remove_file(&stage).unwrap();
    symlink(&outside, stage).unwrap();
    assert!(matches!(
        store.append(upload.id, 0, 3, &b"abc"[..]).await,
        Err(TransferError::Io(_))
    ));
    assert_eq!(fs::read(outside).unwrap(), b"secret");
}

#[tokio::test]
async fn startup_cleans_generated_orphans_and_expired_receipts() {
    let temp = tempfile::tempdir().unwrap();
    let directory = temp.path().join("transfers");
    let store = Uploads::open(&directory).unwrap();
    let upload = store.create(spec(b"")).await.unwrap();
    let prepared = store.prepare_commit(upload.id).await.unwrap();
    fs::rename(
        directory.join(prepared.staging_name()),
        temp.path().join("published"),
    )
    .unwrap();
    prepared.finish().await.unwrap();
    drop(store);

    let manifest = directory.join(format!("{}.json", upload.id));
    let mut json: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
    json["published_at"] = 1.into();
    fs::write(&manifest, serde_json::to_vec(&json).unwrap()).unwrap();
    let orphan = directory.join(format!("{}.upload", Uuid::new_v4()));
    let temp_manifest = directory.join(format!("{}.tmp", Uuid::new_v4()));
    let unrelated = directory.join("keep");
    for path in [&orphan, &temp_manifest, &unrelated] {
        fs::write(path, b"data").unwrap();
    }
    let store = Uploads::open(&directory).unwrap();
    assert!(matches!(
        store.status(upload.id).await,
        Err(TransferError::NotFound)
    ));
    assert!(!orphan.exists());
    assert!(!temp_manifest.exists());
    assert!(unrelated.exists());
}
