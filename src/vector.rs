use std::{fs::File, path::Path};

use memmap2::MmapOptions;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::{Cancellation, Error, Result};

#[derive(Deserialize)]
pub(crate) struct Search {
    needle: Vec<f64>,
    positions: Option<Vec<usize>>,
    rows: usize,
    limit: usize,
}

#[derive(Serialize)]
pub(crate) struct Hit {
    i: usize,
    score: f64,
}

pub(crate) fn rank(path: &Path, search: Search, cancel: &Cancellation) -> Result<Vec<Hit>> {
    let Search {
        needle,
        positions,
        rows,
        limit,
    } = search;
    if needle.is_empty()
        || needle.len() > 65_536
        || !(1..=100).contains(&limit)
        || needle.iter().any(|value| !value.is_finite())
        || positions
            .as_ref()
            .is_some_and(|positions| positions.iter().any(|&index| index >= rows))
    {
        return Err(Error::Invalid("Invalid vector ranking input"));
    }
    let count = positions.as_ref().map_or(rows, Vec::len);
    if count == 0 {
        return Ok(Vec::new());
    }
    let width = needle.len() * size_of::<f32>();
    let bytes = rows
        .checked_mul(width)
        .ok_or(Error::Invalid("Vector file is too large"))?;
    let file = File::open(path).map_err(|_| Error::Io("Could not open the vector index"))?;
    if file
        .metadata()
        .map_err(|_| Error::Io("Could not inspect the vector index"))?
        .len()
        != bytes as u64
    {
        return Err(Error::Invalid(
            "Vector index size does not match its metadata",
        ));
    }
    // The build path supplied by the CLI refers to an immutable, completed artifact.
    let vectors = unsafe { MmapOptions::new().len(bytes).map(&file) }
        .map_err(|_| Error::Io("Could not map the vector index"))?;
    let score = |candidate: usize| {
        cancel.check()?;
        let i = positions
            .as_ref()
            .map_or(candidate, |positions| positions[candidate]);
        let score = vectors[i * width..(i + 1) * width]
            .chunks_exact(4)
            .zip(&needle)
            .map(|(bytes, &query)| {
                let component = f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
                f64::from(component) * query
            })
            .sum::<f64>();
        if !score.is_finite() {
            return Err(Error::Invalid("Invalid vector value in the index"));
        }
        Ok(Hit { i, score })
    };
    let candidates = 0..count;
    let mut ranked: Vec<Hit> = if count.saturating_mul(needle.len()) >= 1_000_000 {
        candidates
            .into_par_iter()
            .map(score)
            .collect::<Result<_>>()?
    } else {
        candidates.map(score).collect::<Result<_>>()?
    };
    let order = |a: &Hit, b: &Hit| b.score.total_cmp(&a.score).then(a.i.cmp(&b.i));
    if ranked.len() > limit {
        ranked.select_nth_unstable_by(limit, order);
        ranked.truncate(limit);
    }
    ranked.sort_unstable_by(order);
    Ok(ranked)
}
