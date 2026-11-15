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

const LANES: usize = 8;
const PARALLEL_COMPONENTS: usize = 1_000_000;
const COMPONENTS_PER_CHECK: usize = 65_536;
const ROWS_PER_CHECK: usize = 256;

#[derive(Clone, Default)]
struct Candidate {
    i: usize,
    score: f64,
    error: f64,
}

pub(crate) fn rank(path: &Path, search: Search, cancel: &Cancellation) -> Result<Vec<Hit>> {
    cancel.check()?;
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
    {
        return Err(Error::Invalid("Invalid vector ranking input"));
    }
    if let Some(positions) = &positions {
        for block in positions.chunks(8192) {
            cancel.check()?;
            if block.iter().any(|&index| index >= rows) {
                return Err(Error::Invalid("Invalid vector ranking input"));
            }
        }
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
    let dimensions = needle.len();
    let components = vectors.as_chunks::<4>().0;
    let row = |i| &components[i * dimensions..(i + 1) * dimensions];
    let query: Vec<f32> = needle.iter().map(|&value| value as f32).collect();
    let use_f32 = count > limit && query.iter().all(|value| value.is_finite());
    // Bound query conversion and dot-product rounding by the longest accumulation path.
    // EPSILON is twice the round-to-nearest unit roundoff and leaves room for compounded
    // rounding and the f64 reference calculation. Also allow for subnormal arithmetic.
    let steps = dimensions.div_ceil(LANES) + LANES + 3;
    let underflow = f64::from(f32::MIN_POSITIVE) * (2 * dimensions + LANES) as f64;
    let relative = f64::from(f32::EPSILON) * steps as f64;
    let l1 = needle.iter().map(|value| value.abs()).sum::<f64>();
    let error_per_peak = relative * l1 + underflow;
    let mut ranked = vec![Candidate::default(); count];
    visit_blocks(&mut ranked, dimensions, cancel, |start, block| {
        for (offset, hit) in block.iter_mut().enumerate() {
            let candidate = start + offset;
            let i = positions
                .as_ref()
                .map_or(candidate, |positions| positions[candidate]);
            let (score, error) = if use_f32 {
                let (score, peak) = approximate_score(row(i), &query)?;
                if score.is_finite() {
                    (
                        f64::from(score),
                        f64::from(peak) * error_per_peak + underflow,
                    )
                } else {
                    // An overflowing f32 estimate cannot exclude this candidate.
                    (0.0, f64::INFINITY)
                }
            } else {
                (precise_score(row(i), &needle)?, 0.0)
            };
            *hit = Candidate { i, score, error };
        }
        Ok(())
    })?;
    cancel.check()?;
    if use_f32 {
        let lower = |hit: &Candidate| hit.score - hit.error;
        let (_, boundary, _) =
            ranked.select_nth_unstable_by(limit - 1, |a, b| lower(b).total_cmp(&lower(a)));
        let threshold = lower(boundary);
        // Only discard rows that cannot reach the top k, even with rounding error.
        ranked.retain(|hit| hit.score + hit.error >= threshold);
        visit_blocks(&mut ranked, dimensions, cancel, |_, block| {
            for hit in block {
                hit.score = precise_score(row(hit.i), &needle)?;
            }
            Ok(())
        })?;
    }
    cancel.check()?;
    let order = |a: &Candidate, b: &Candidate| b.score.total_cmp(&a.score).then(a.i.cmp(&b.i));
    if ranked.len() > limit {
        ranked.select_nth_unstable_by(limit, order);
        ranked.truncate(limit);
    }
    ranked.sort_unstable_by(order);
    cancel.check()?;
    Ok(ranked
        .into_iter()
        .map(|Candidate { i, score, .. }| Hit { i, score })
        .collect())
}

fn visit_blocks(
    hits: &mut [Candidate],
    dimensions: usize,
    cancel: &Cancellation,
    visit: impl Fn(usize, &mut [Candidate]) -> Result<()> + Sync,
) -> Result<()> {
    let rows_per_block = (COMPONENTS_PER_CHECK / dimensions).clamp(1, ROWS_PER_CHECK);
    let run = |(block, hits): (usize, &mut [Candidate])| {
        cancel.check()?;
        visit(block * rows_per_block, hits)
    };
    if hits.len().saturating_mul(dimensions) >= PARALLEL_COMPONENTS {
        hits.par_chunks_mut(rows_per_block)
            .enumerate()
            .try_for_each(run)
    } else {
        hits.chunks_mut(rows_per_block)
            .enumerate()
            .try_for_each(run)
    }
}

fn approximate_score(row: &[[u8; 4]], query: &[f32]) -> Result<(f32, f32)> {
    let mut sums = [0.0_f32; LANES];
    let mut peaks = [0_u32; LANES];
    let (blocks, tail) = row.as_chunks::<LANES>();
    let (query_blocks, query_tail) = query.as_chunks::<LANES>();
    for (block, query) in blocks.iter().zip(query_blocks) {
        for lane in 0..LANES {
            let bits = u32::from_le_bytes(block[lane]);
            sums[lane] += f32::from_bits(bits) * query[lane];
            peaks[lane] = peaks[lane].max(bits & 0x7fff_ffff);
        }
    }
    for (lane, (&bytes, &query)) in tail.iter().zip(query_tail).enumerate() {
        let bits = u32::from_le_bytes(bytes);
        sums[lane] += f32::from_bits(bits) * query;
        peaks[lane] = peaks[lane].max(bits & 0x7fff_ffff);
    }
    let peak = f32::from_bits(peaks.into_iter().max().unwrap_or_default());
    if !peak.is_finite() {
        return Err(Error::Invalid("Invalid vector value in the index"));
    }
    Ok((sums.into_iter().sum(), peak))
}

fn precise_score(row: &[[u8; 4]], query: &[f64]) -> Result<f64> {
    let score = row
        .iter()
        .zip(query)
        .map(|(&bytes, &query)| f64::from(f32::from_le_bytes(bytes)) * query)
        .sum::<f64>();
    if !score.is_finite() {
        return Err(Error::Invalid("Invalid vector value in the index"));
    }
    Ok(score)
}
