use std::fs;
use std::sync::Barrier;

use dunlin::{Cancellation, Error, KnowledgeBase};

#[test]
fn simultaneous_first_opens_share_one_index() {
    let root = tempfile::tempdir().unwrap();
    let index = root.path().join("index");
    let ready = Barrier::new(3);
    let (first, second) = std::thread::scope(|scope| {
        let open = || {
            ready.wait();
            KnowledgeBase::open(&index, &Cancellation::default()).unwrap()
        };
        let first = scope.spawn(open);
        let second = scope.spawn(open);
        ready.wait();
        (first.join().unwrap(), second.join().unwrap())
    });
    let source = root.path().join("source.md");
    fs::write(&source, "# Authentication\nOAuth").unwrap();
    let cancel = Cancellation::default();
    first.add_source("docs", source, &cancel).unwrap();
    drop(second);
    let reopened = KnowledgeBase::open(&index, &cancel).unwrap();
    let hits = reopened.search("OAuth", 1, &cancel).unwrap().hits;
    assert_eq!(hits.len(), 1);
    assert_eq!(
        reopened.read(&hits[0].evidence, &cancel).unwrap().content,
        "# Authentication\nOAuth"
    );
    cancel.abort();
    assert!(matches!(
        reopened.search("OAuth", 1, &cancel),
        Err(Error::Aborted)
    ));
}
