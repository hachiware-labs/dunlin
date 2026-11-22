//! The store with a list that holds information objects instead of documents
//! (docs/data-management.md, 4.5 and 6.4): an `index` snapshot lists what each index is.

use dunlin::records::{InfoId, ModuleIdentity, Process, info_id, object_id};
use dunlin::store::{Committed, Outcome, SnapshotId, Store};
use serde_json::{Value, json};

fn indexing(tokenizer: &str) -> Process {
    Process {
        modules: vec![ModuleIdentity {
            name: "index.keyword".into(),
            compatibility: 1,
            args: json!({ "tokenizer": tokenizer }),
        }],
    }
}

/// What a keyword index is: what it was made from, and by which modules with which arguments.
fn keyword_info(store: &Store, chunks: SnapshotId, tokenizer: &str) -> Value {
    json!({
        "index": "keyword",
        "input": store.snapshot(chunks).unwrap().digest().as_str(),
        "modules": indexing(tokenizer).to_value(),
    })
}

/// Runs the `index` stage for a tokenizer, as the stage will: look the key up, and otherwise
/// list one information object under the kind of index.
fn index(store: &Store, chunks: SnapshotId, tokenizer: &str, tag: &str) -> Committed {
    let mut draft = store
        .begin("index", "index", &[chunks], &indexing(tokenizer))
        .unwrap();
    if let Some(existing) = draft.existing().unwrap() {
        return draft.finish_reused(existing, Some(tag)).unwrap();
    }
    let info = draft
        .save_info(&keyword_info(store, chunks, tokenizer))
        .unwrap();
    draft.list(vec!["keyword".into()], info.to_string());
    draft.commit(Some(tag), None).unwrap()
}

#[test]
fn lists_what_an_index_is() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(root.path()).unwrap();

    // Chunks of two documents, as the stage before would leave them.
    let mut draft = store.begin_scan("transform", "chunks").unwrap();
    for document in ["a.md", "b.md"] {
        let chunks = draft.save(document.as_bytes()).unwrap();
        draft.list(vec!["docs".into(), document.into()], chunks.to_string());
    }
    let chunks = draft.commit(Some("chunks-800"), None).unwrap().snapshot;

    // The list of an index snapshot holds one entry per index, not one per document.
    let main = index(&store, chunks, "lindera", "main");
    assert_eq!(main.outcome, Outcome::Created);
    let snapshot = store.snapshot(main.snapshot).unwrap();
    assert_eq!(snapshot.kind, "index");
    assert_eq!(snapshot.parents, [chunks]);
    assert_eq!(snapshot.items.len(), 1);
    let (name, id) = &snapshot.items[0];
    assert_eq!(name, &["keyword"]);
    let info = InfoId::parse(id).expect("an information object");
    assert_eq!(info, info_id(&keyword_info(&store, chunks, "lindera")));
    assert_eq!(
        store.info(&info).unwrap(),
        keyword_info(&store, chunks, "lindera")
    );
    // The documents are those of the parent.
    assert_eq!(store.snapshot(chunks).unwrap().items.len(), 2);

    // Another tokenizer over the same chunks is another index, so another version.
    let trial = index(&store, chunks, "bigram", "trial-bigram");
    assert_eq!(trial.outcome, Outcome::Created);
    assert_ne!(trial.snapshot, main.snapshot);
    assert_ne!(
        store.snapshot(trial.snapshot).unwrap().digest(),
        snapshot.digest()
    );
    assert_eq!(store.resolve("index", "main").unwrap(), Some(main.snapshot));
    assert_eq!(
        store.resolve("index", "trial-bigram").unwrap(),
        Some(trial.snapshot)
    );

    // Where the index was built is a note on the information object. It changes neither the
    // object's ID nor the snapshot, and it can be written again when the index is rebuilt.
    assert_eq!(store.object_notes(info.as_str()).unwrap(), json!({}));
    let built = |generation: u32| json!({ "location": "indexes/main", "generation": generation });
    store
        .note_object(info.as_str(), "built", &built(1))
        .unwrap();
    store
        .note_object(info.as_str(), "built", &built(2))
        .unwrap();
    assert_eq!(
        store.object_notes(info.as_str()).unwrap(),
        json!({ "built": built(2) })
    );
    assert_eq!(store.snapshot(main.snapshot).unwrap(), snapshot);
    assert_eq!(
        store
            .object_notes(&store.snapshot(trial.snapshot).unwrap().items[0].1)
            .unwrap(),
        json!({})
    );

    // The same chunks and settings again: found by the key, and the note is still there.
    let again = index(&store, chunks, "lindera", "main");
    assert_eq!(again.outcome, Outcome::Reused);
    assert_eq!(again.snapshot, main.snapshot);
    assert_eq!(
        store.object_notes(info.as_str()).unwrap()["built"]["generation"],
        2
    );

    // A document that happens to hold the same JSON is another object: it does not get the note.
    let same_bytes = dunlin::records::canonical(&keyword_info(&store, chunks, "lindera"));
    assert_ne!(
        object_id(same_bytes.as_bytes()).as_str()[4..],
        info.as_str()[4..]
    );
    assert!(
        store
            .note_object("obj-missing", "built", &built(1))
            .is_err()
    );
}
