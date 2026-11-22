use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;
use std::time::Instant;

use percent_encoding::percent_decode_str;
use pulldown_cmark::{Event, Parser, Tag};
use serde::Serialize;
use url::Url;

use crate::engine::KeywordSnapshot;
use crate::{
    Cancellation, Error, Evidence, ReadOutcome, Result, RetrievalMethod, SourceRef, source,
};

const MAX_LINKS_PER_DOCUMENT: usize = 16;

pub struct ExploreOptions {
    pub max_steps: usize,
    pub timeout_ms: u32,
    pub seed_limit: Option<usize>,
    pub source_ids: Vec<String>,
}

impl Default for ExploreOptions {
    fn default() -> Self {
        Self {
            max_steps: 12,
            timeout_ms: 1_000,
            seed_limit: None,
            source_ids: Vec::new(),
        }
    }
}

impl ExploreOptions {
    pub(crate) fn validate(&self) -> Result<()> {
        if !(1..=100).contains(&self.max_steps) || !(1..=60_000).contains(&self.timeout_ms) {
            return Err(Error::Invalid(
                "Explore maxSteps must be 1–100 and timeoutMs 1–60000",
            ));
        }
        if self
            .seed_limit
            .is_some_and(|limit| !(1..=100).contains(&limit))
        {
            return Err(Error::Invalid("Explore seedLimit must be 1–100"));
        }
        source::validate_scope(&self.source_ids)
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExploreStop {
    Exhausted,
    StepLimit,
    Timeout,
    Cancelled,
    SearchFailed,
}

impl ExploreStop {
    fn from_interrupt(error: Error) -> Result<Self> {
        match error {
            Error::Aborted => Ok(Self::Cancelled),
            Error::Timeout => Ok(Self::Timeout),
            error => Err(error),
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExploreOutcome {
    pub evidence: Vec<ExploreEvidence>,
    pub issues: Vec<ExploreIssue>,
    pub steps: usize,
    pub plan_id: String,
    pub status: &'static str,
    pub stop_reason: ExploreStop,
    pub elapsed_ms: f64,
}

#[derive(Debug, Serialize)]
pub struct ExploreEvidence {
    pub evidence: Evidence,
    pub via: ExploreVia,
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExploreVia {
    Search,
    Link {
        from: SourceRef,
        line: usize,
        destination: String,
    },
}

#[derive(Debug, Serialize)]
pub struct ExploreIssue {
    pub code: &'static str,
    pub from: SourceRef,
    pub line: Option<usize>,
    pub destination: Option<String>,
}

pub(crate) fn explore(
    snapshot: &KeywordSnapshot<'_>,
    query: &str,
    route: (String, RetrievalMethod, usize),
    options: ExploreOptions,
    cancel: &Cancellation,
    start: Instant,
) -> Result<ExploreOutcome> {
    let (plan_id, method, default_limit) = route;
    let mut walk = Walk::new(&options.source_ids);
    let stop_reason = match walk.run(
        snapshot,
        method,
        query,
        options.seed_limit.unwrap_or(default_limit),
        options.max_steps,
        cancel,
    ) {
        Ok(reason) => reason,
        Err(error) => ExploreStop::from_interrupt(error)?,
    };
    let status = match stop_reason {
        ExploreStop::Exhausted if walk.issues.is_empty() && walk.evidence.is_empty() => {
            "no_matches"
        }
        ExploreStop::Exhausted if walk.issues.is_empty() => "complete",
        _ => "partial",
    };
    Ok(ExploreOutcome {
        evidence: walk.evidence,
        issues: walk.issues,
        steps: walk.steps,
        plan_id,
        status,
        stop_reason,
        elapsed_ms: start.elapsed().as_secs_f64() * 1000.0,
    })
}

enum Work {
    Inspect(SourceRef),
    Follow(Link),
}

struct Link {
    from: SourceRef,
    line: usize,
    destination: String,
    target: LinkTarget,
}

#[derive(Clone, Eq, Hash, PartialEq)]
enum LinkTarget {
    Object {
        source_id: String,
        object_id: String,
    },
    File {
        url: String,
        local_id: Option<String>,
    },
}

struct Walk<'a> {
    sources: &'a [String],
    queue: VecDeque<Work>,
    seen: HashSet<(String, String)>,
    evidence: Vec<ExploreEvidence>,
    issues: Vec<ExploreIssue>,
    steps: usize,
}

impl<'a> Walk<'a> {
    fn new(sources: &'a [String]) -> Self {
        Self {
            sources,
            queue: VecDeque::new(),
            seen: HashSet::new(),
            evidence: Vec::new(),
            issues: Vec::new(),
            steps: 0,
        }
    }

    fn run(
        &mut self,
        snapshot: &KeywordSnapshot<'_>,
        method: RetrievalMethod,
        query: &str,
        seed_limit: usize,
        max_steps: usize,
        cancel: &Cancellation,
    ) -> Result<ExploreStop> {
        cancel.check()?;
        self.steps += 1;
        let sources = self.sources;
        let search = snapshot.visit_hits(method, query, seed_limit, sources, cancel, |hit| {
            if self.mark_seen(&hit.evidence.source) {
                self.queue
                    .push_back(Work::Inspect(hit.evidence.source.clone()));
                self.evidence.push(ExploreEvidence {
                    evidence: hit.evidence,
                    via: ExploreVia::Search,
                });
            }
        });
        match search {
            Err(Error::Index) if !self.evidence.is_empty() => return Ok(ExploreStop::SearchFailed),
            result => {
                result?;
            }
        }
        loop {
            cancel.check()?;
            let Some(work) = self.queue.pop_front() else {
                return Ok(ExploreStop::Exhausted);
            };
            if let Work::Follow(link) = &work
                && self.target_seen(&link.from, &link.target)
            {
                continue;
            }
            if self.steps == max_steps {
                return Ok(ExploreStop::StepLimit);
            }
            self.steps += 1;
            match work {
                Work::Inspect(from) => match snapshot.object(&from.source_id, &from.object_id) {
                    Ok(Some(object)) => self.inspect(&object, cancel)?,
                    _ => self.issue("read_failed", from, None, None),
                },
                Work::Follow(link) => self.follow(snapshot, link, cancel)?,
            }
        }
    }

    fn mark_seen(&mut self, source: &SourceRef) -> bool {
        self.seen
            .insert((source.source_id.clone(), source.object_id.clone()))
    }

    fn inspect(&mut self, object: &ReadOutcome, cancel: &Cancellation) -> Result<()> {
        let from = &object.evidence.source;
        if !from
            .object_id
            .rsplit_once('.')
            .is_some_and(|(_, ext)| ext.eq_ignore_ascii_case("md"))
        {
            return Ok(());
        }
        cancel.check()?;
        let Ok(base) = Url::parse(&from.url) else {
            self.issue("read_failed", from.clone(), None, None);
            return Ok(());
        };
        let mut seen_links = HashSet::new();
        for (event, offset) in Parser::new(&object.content).into_offset_iter() {
            cancel.check()?;
            let Event::Start(Tag::Link { dest_url, .. }) = event else {
                continue;
            };
            let Some(target) = parse_link(&base, from, &dest_url, self.sources) else {
                continue;
            };
            if self.target_seen(from, &target) || !seen_links.insert(target.clone()) {
                continue;
            }
            if seen_links.len() > MAX_LINKS_PER_DOCUMENT {
                self.issue("link_limit", from.clone(), None, None);
                break;
            }
            let line = object.content.as_bytes()[..offset.start]
                .iter()
                .filter(|&&byte| byte == b'\n')
                .count()
                + 1;
            self.queue.push_back(Work::Follow(Link {
                from: from.clone(),
                line,
                destination: dest_url.into_string(),
                target,
            }));
        }
        Ok(())
    }

    fn target_seen(&self, from: &SourceRef, target: &LinkTarget) -> bool {
        match target {
            LinkTarget::Object {
                source_id,
                object_id,
            } => self.seen.contains(&(source_id.clone(), object_id.clone())),
            LinkTarget::File {
                local_id: Some(id), ..
            } => self.seen.contains(&(from.source_id.clone(), id.clone())),
            LinkTarget::File { local_id: None, .. } => false,
        }
    }

    fn follow(
        &mut self,
        snapshot: &KeywordSnapshot<'_>,
        link: Link,
        cancel: &Cancellation,
    ) -> Result<()> {
        let object = match &link.target {
            LinkTarget::Object {
                source_id,
                object_id,
            } => snapshot.object(source_id, object_id),
            LinkTarget::File { url, local_id } => {
                let local = match local_id {
                    Some(id) => snapshot.object(&link.from.source_id, id),
                    None => Ok(None),
                };
                match local {
                    Ok(None) => snapshot.object_by_url(url, self.sources, cancel),
                    result => result,
                }
            }
        };
        cancel.check()?;
        let object = match object {
            Ok(Some(object)) => object,
            Ok(None) => {
                self.issue(
                    "unresolved_link",
                    link.from,
                    Some(link.line),
                    Some(link.destination),
                );
                return Ok(());
            }
            Err(_) => {
                self.issue(
                    "read_failed",
                    link.from,
                    Some(link.line),
                    Some(link.destination),
                );
                return Ok(());
            }
        };
        if object.evidence.range.end_line == 0 {
            self.issue(
                "empty_target",
                link.from,
                Some(link.line),
                Some(link.destination),
            );
            return Ok(());
        }
        if self.mark_seen(&object.evidence.source) {
            let mut evidence = object.evidence.clone();
            evidence.range.end_line = evidence.range.end_line.min(source::WINDOW_LINES);
            self.evidence.push(ExploreEvidence {
                evidence,
                via: ExploreVia::Link {
                    from: link.from,
                    line: link.line,
                    destination: link.destination,
                },
            });
            self.inspect(&object, cancel)?;
        }
        Ok(())
    }

    fn issue(
        &mut self,
        code: &'static str,
        from: SourceRef,
        line: Option<usize>,
        destination: Option<String>,
    ) {
        self.issues.push(ExploreIssue {
            code,
            from,
            line,
            destination,
        });
    }
}

fn parse_link(
    base: &Url,
    from: &SourceRef,
    destination: &str,
    sources: &[String],
) -> Option<LinkTarget> {
    if destination.is_empty() || destination.starts_with('#') || destination.len() > 4096 {
        return None;
    }
    if let Some(address) = destination.strip_prefix("knowledge-nest://") {
        let path = address.split(['#', '?']).next()?;
        let (id, object_id) = path.split_once('/')?;
        if !source::in_scope(id, sources) || source::validate_id(id).is_err() {
            return None;
        }
        let object_id = percent_decode_str(object_id).decode_utf8().ok()?;
        if object_id
            .split('/')
            .any(|part| matches!(part, "" | "." | "..") || part.contains(['\\', '\0']))
        {
            return None;
        }
        return Some(LinkTarget::Object {
            source_id: id.into(),
            object_id: object_id.into_owned(),
        });
    }
    let target = file_path(&base.join(destination).ok()?)?;
    let root_path = file_path(base)?;
    let root = root_path
        .ancestors()
        .nth(from.object_id.split('/').count())?;
    let local_id = target
        .strip_prefix(root)
        .ok()
        .and_then(|path| source::object_id(path).ok());
    let url = Url::from_file_path(target).ok()?;
    Some(LinkTarget::File {
        url: url.into(),
        local_id,
    })
}

fn file_path(url: &Url) -> Option<PathBuf> {
    if url.scheme() != "file" {
        return None;
    }
    url.to_file_path().ok()
}
