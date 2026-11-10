use arcdos_vector::{Chunk, EmbeddingStore, ExactIndex, Model, VectorIndex};

#[test]
fn rebuild_reads_persistent_model_version_and_source_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("embeddings.db");
    let original = Model::new("fixture", "v1", 2).unwrap();
    let replacement = Model::new("fixture", "v2", 3).unwrap();
    let mut store = EmbeddingStore::open(&db).unwrap();
    store
        .replace_file(&original, "file-a", "revision-1", &[chunk(2, &[1.0, 0.0])])
        .unwrap();
    store
        .replace_file(
            &replacement,
            "file-a",
            "revision-1",
            &[chunk(0, &[0.0, 0.0, 1.0])],
        )
        .unwrap();
    store
        .replace_file(&original, "file-b", "revision-2", &[chunk(0, &[0.0, 1.0])])
        .unwrap();
    drop(store);

    let mut store = EmbeddingStore::open(&db).unwrap();
    let wrong_dimensions = Model::new("fixture", "v1", 3).unwrap();
    assert!(
        store
            .rebuild(&wrong_dimensions, ExactIndex::new(3).unwrap())
            .is_err()
    );
    let snapshot = store
        .rebuild(&original, ExactIndex::new(2).unwrap())
        .unwrap();
    let hits = snapshot.search(&[1.0, 0.0], 2).unwrap();
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].source.file_id, "file-a");
    assert_eq!(hits[0].source.revision, "revision-1");
    assert_eq!(hits[0].source.position, 2);
    assert_eq!(hits[0].source.content_hash, [2; 32]);
    assert_eq!(hits[1].source.file_id, "file-b");

    store.remove_file("file-a").unwrap();
    let snapshot = store
        .rebuild(&original, ExactIndex::new(2).unwrap())
        .unwrap();
    assert_eq!(snapshot.search(&[1.0, 0.0], 2).unwrap().len(), 1);
    let snapshot = store
        .rebuild(&replacement, ExactIndex::new(3).unwrap())
        .unwrap();
    assert!(snapshot.search(&[1.0, 0.0, 0.0], 2).unwrap().is_empty());
}

#[test]
fn invalid_replacement_keeps_previous_revision_and_valid_replacement_removes_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = EmbeddingStore::open(&dir.path().join("embeddings.db")).unwrap();
    let model = Model::new("fixture", "v1", 2).unwrap();
    store
        .replace_file(&model, "file", "old", &[chunk(0, &[1.0, 0.0])])
        .unwrap();
    for invalid in [
        chunk(1, &[f32::NAN, 0.0]),
        chunk(1, &[0.0, 0.0]),
        chunk(1, &[1.0]),
    ] {
        assert!(
            store
                .replace_file(&model, "file", "new", &[chunk(0, &[0.0, 1.0]), invalid])
                .is_err()
        );
    }
    assert!(
        store
            .replace_file(
                &model,
                "file",
                "new",
                &[chunk(0, &[1.0, 0.0]), chunk(0, &[0.0, 1.0])]
            )
            .is_err()
    );
    let old = store.rebuild(&model, ExactIndex::new(2).unwrap()).unwrap();
    assert_eq!(
        old.search(&[1.0, 0.0], 1).unwrap()[0].source.revision,
        "old"
    );

    store
        .replace_file(&model, "file", "new", &[chunk(1, &[0.0, 1.0])])
        .unwrap();
    let new = store.rebuild(&model, ExactIndex::new(2).unwrap()).unwrap();
    let hits = new.search(&[0.0, 1.0], 2).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].source.revision, "new");
    assert_eq!(hits[0].source.position, 1);
}

#[test]
fn exact_cosine_ranks_ties_deterministically_and_rejects_invalid_queries() {
    let mut index = ExactIndex::new(2).unwrap();
    index.add(3, &[f32::MAX, 0.0]).unwrap();
    index.add(1, &[f32::from_bits(1), 0.0]).unwrap();
    index.add(2, &[-1.0, 0.0]).unwrap();
    assert!(index.add(1, &[0.0, 1.0]).is_err());
    let hits = index.search(&[1.0, 0.0], 2).unwrap();
    assert_eq!(hits.iter().map(|hit| hit.key).collect::<Vec<_>>(), [1, 3]);
    assert_eq!(hits[0].distance, 0.0);
    assert!(index.search(&[1.0, 0.0], 0).is_err());
    assert!(index.search(&[0.0, 0.0], 1).is_err());
    assert!(index.search(&[0.0, f32::INFINITY], 1).is_err());
    assert!(index.search(&[1.0], 1).is_err());
}

#[cfg(feature = "usearch")]
#[test]
fn usearch_rebuilds_from_the_same_source_database() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = EmbeddingStore::open(&dir.path().join("embeddings.db")).unwrap();
    let model = Model::new("fixture", "v1", 2).unwrap();
    store
        .replace_file(
            &model,
            "file-a",
            "r1",
            &[chunk(0, &[0.0, 1.0]), chunk(1, &[1.0, 0.0])],
        )
        .unwrap();
    let index = arcdos_vector::UsearchIndex::new(2, 1).unwrap();
    let snapshot = store.rebuild(&model, index).unwrap();
    let results = snapshot.search(&[1.0, 0.0], 2).unwrap();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].source.position, 1);
    assert!(results[0].distance < results[1].distance);
}

fn chunk(position: u32, embedding: &[f32]) -> Chunk<'_> {
    Chunk {
        position,
        content_hash: [position as u8; 32],
        embedding,
    }
}
