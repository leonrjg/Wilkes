use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};

use crate::types::BookmarkClusterGranularity;

#[derive(Clone, Debug, PartialEq)]
pub struct EmbeddingCluster {
    pub item_indices: Vec<usize>,
    pub representative_index: usize,
    pub cohesion: f32,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct EmbeddingClusterResult {
    pub clusters: Vec<EmbeddingCluster>,
    pub unclustered_indices: Vec<usize>,
}

#[derive(Clone, Copy, Debug)]
struct WardMerge {
    left: usize,
    right: usize,
}

/// A complete deterministic Ward dendrogram plus the pairwise similarities
/// needed to evaluate and describe any supported cut.
///
/// Building owns the O(n²) work. Calling [`Self::cut`] only replays merge ids
/// and evaluates the selected partition, so callers can retain one tree while
/// users adjust granularity.
#[derive(Debug)]
pub struct WardTree {
    item_count: usize,
    merges: Vec<WardMerge>,
    item_similarities: Vec<Vec<f32>>,
}

#[derive(Clone, Copy, Debug)]
struct WardEdge {
    cost: f32,
    left: usize,
    right: usize,
}

impl PartialEq for WardEdge {
    fn eq(&self, other: &Self) -> bool {
        self.cost.to_bits() == other.cost.to_bits()
            && self.left == other.left
            && self.right == other.right
    }
}

impl Eq for WardEdge {}

impl PartialOrd for WardEdge {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for WardEdge {
    fn cmp(&self, other: &Self) -> Ordering {
        // BinaryHeap is a max-heap, so reverse the cost ordering.
        other
            .cost
            .total_cmp(&self.cost)
            // Prefer the lexicographically smaller pair when costs tie.
            .then_with(|| other.left.cmp(&self.left))
            .then_with(|| other.right.cmp(&self.right))
    }
}

/// Cluster embedding vectors with deterministic Ward-link agglomeration.
///
/// Ward's merge objective minimizes the increase in within-cluster dispersion,
/// preventing a small outlier group from leaving the rest of a collection in a
/// single catch-all cluster. Granularity selects a deterministic cut based on
/// collection size; silhouette quality is only used to reject a cut with no
/// positive structure. Singleton groups are returned as unclustered items.
pub fn cluster_embeddings(
    vectors: &[Vec<f32>],
    granularity: BookmarkClusterGranularity,
) -> anyhow::Result<EmbeddingClusterResult> {
    WardTree::build(vectors)?.cut(granularity)
}

impl WardTree {
    /// Build the full Ward merge tree once. Continuing from the coarsest UI cut
    /// down to one root does not alter any earlier merge, so every granularity
    /// observes exactly the same deterministic sequence.
    pub fn build(vectors: &[Vec<f32>]) -> anyhow::Result<Self> {
        Self::build_with_cancel(vectors, &AtomicBool::new(false))
    }

    /// Build while cooperatively observing cancellation from a blocking
    /// caller. The ordinary builder delegates here so clustering has one
    /// implementation and identical merge semantics in every context.
    pub fn build_with_cancel(vectors: &[Vec<f32>], cancelled: &AtomicBool) -> anyhow::Result<Self> {
        let count = vectors.len();
        if count < 3 {
            return Ok(Self {
                item_count: count,
                merges: Vec::new(),
                item_similarities: Vec::new(),
            });
        }

        let dimension = vectors.first().map(Vec::len).unwrap_or_default();
        anyhow::ensure!(dimension > 0, "Cannot cluster zero-dimensional embeddings");
        anyhow::ensure!(
            vectors.iter().all(|vector| vector.len() == dimension),
            "Bookmark embedding dimensions do not match"
        );
        anyhow::ensure!(
            vectors.iter().flatten().all(|value| value.is_finite()),
            "Bookmark embeddings contain non-finite values"
        );

        let mut normalized = Vec::with_capacity(count);
        for vector in vectors {
            ensure_not_cancelled(cancelled)?;
            normalized.push(normalize(vector));
        }
        let squared_norms: Vec<f32> = normalized
            .iter()
            .map(|vector| dot(vector, vector))
            .collect();
        let capacity = count * 2 - 1;
        let mut centroids: Vec<Vec<f32>> = vec![Vec::new(); capacity];
        let mut sizes = vec![0usize; capacity];
        let mut active = vec![false; capacity];
        let mut heap = BinaryHeap::new();
        let mut item_similarities = vec![vec![0.0; count]; count];
        for index in 0..count {
            ensure_not_cancelled(cancelled)?;
            item_similarities[index][index] = 1.0;
            centroids[index] = normalized[index].clone();
            sizes[index] = 1;
            active[index] = true;
            for other in (index + 1)..count {
                if other.is_multiple_of(256) {
                    ensure_not_cancelled(cancelled)?;
                }
                let similarity = dot(&normalized[index], &normalized[other]).clamp(-1.0, 1.0);
                item_similarities[index][other] = similarity;
                item_similarities[other][index] = similarity;
                heap.push(WardEdge {
                    cost: singleton_ward_merge_cost(
                        squared_norms[index],
                        squared_norms[other],
                        similarity,
                    ),
                    left: index,
                    right: other,
                });
            }
        }

        let mut merges = Vec::with_capacity(count - 1);
        let mut active_count = count;
        let mut next_cluster = count;

        while active_count > 1 {
            ensure_not_cancelled(cancelled)?;
            let Some(edge) = heap.pop() else {
                break;
            };
            if !active[edge.left] || !active[edge.right] {
                continue;
            }

            let merged = next_cluster;
            next_cluster += 1;
            let left_size = sizes[edge.left];
            let right_size = sizes[edge.right];
            sizes[merged] = left_size + right_size;
            centroids[merged] = centroids[edge.left]
                .iter()
                .zip(&centroids[edge.right])
                .map(|(left, right)| {
                    (left * left_size as f32 + right * right_size as f32) / sizes[merged] as f32
                })
                .collect();

            active[edge.left] = false;
            active[edge.right] = false;
            active[merged] = true;
            active_count -= 1;
            merges.push(WardMerge {
                left: edge.left,
                right: edge.right,
            });

            for other in 0..merged {
                if other.is_multiple_of(256) {
                    ensure_not_cancelled(cancelled)?;
                }
                if !active[other] {
                    continue;
                }
                heap.push(WardEdge {
                    cost: ward_merge_cost(
                        &centroids[other],
                        sizes[other],
                        &centroids[merged],
                        sizes[merged],
                    ),
                    left: other,
                    right: merged,
                });
            }
        }

        anyhow::ensure!(
            active_count == 1 && merges.len() == count - 1,
            "Could not produce the complete Ward tree"
        );
        Ok(Self {
            item_count: count,
            merges,
            item_similarities,
        })
    }

    /// Recut this tree without rebuilding embeddings, distances, or the Ward
    /// heap. Silhouette rejection and representative selection remain specific
    /// to the requested cut.
    pub fn cut(
        &self,
        granularity: BookmarkClusterGranularity,
    ) -> anyhow::Result<EmbeddingClusterResult> {
        self.cut_with_cancel(granularity, &AtomicBool::new(false))
    }

    /// Recut while cooperatively observing cancellation during the retained
    /// O(n²) quality and representative scans.
    pub fn cut_with_cancel(
        &self,
        granularity: BookmarkClusterGranularity,
        cancelled: &AtomicBool,
    ) -> anyhow::Result<EmbeddingClusterResult> {
        let count = self.item_count;
        if count < 3 {
            return Ok(EmbeddingClusterResult {
                clusters: Vec::new(),
                unclustered_indices: (0..count).collect(),
            });
        }

        let target_cluster_count = cluster_count_for_granularity(count, granularity);
        let capacity = count * 2 - 1;
        let mut active = vec![false; capacity];
        active[..count].fill(true);
        for (merge_index, merge) in self
            .merges
            .iter()
            .take(count - target_cluster_count)
            .enumerate()
        {
            ensure_not_cancelled(cancelled)?;
            anyhow::ensure!(
                active[merge.left] && active[merge.right],
                "Ward tree contains an invalid merge sequence"
            );
            active[merge.left] = false;
            active[merge.right] = false;
            active[count + merge_index] = true;
        }

        let mut partition: Vec<Vec<usize>> = active
            .iter()
            .enumerate()
            .filter(|(_, is_active)| **is_active)
            .map(|(node, _)| self.leaf_members(node))
            .collect();
        anyhow::ensure!(
            partition.len() == target_cluster_count,
            "Could not produce the requested Ward tree cut"
        );
        partition.sort_by_key(|group| group[0]);
        if silhouette_score(&partition, &self.item_similarities, cancelled)? <= f32::EPSILON {
            return Ok(EmbeddingClusterResult {
                clusters: Vec::new(),
                unclustered_indices: (0..count).collect(),
            });
        }

        let mut result = EmbeddingClusterResult::default();
        for group in partition {
            ensure_not_cancelled(cancelled)?;
            if group.len() < 2 {
                result.unclustered_indices.extend(group);
                continue;
            }
            let representative_index = medoid(&group, &self.item_similarities, cancelled)?;
            let cohesion = average_pair_similarity(&group, &self.item_similarities, cancelled)?;
            result.clusters.push(EmbeddingCluster {
                item_indices: group,
                representative_index,
                cohesion,
            });
        }
        result
            .clusters
            .sort_by_key(|cluster| cluster.item_indices[0]);
        result.unclustered_indices.sort_unstable();
        Ok(result)
    }

    fn leaf_members(&self, node: usize) -> Vec<usize> {
        let mut members = Vec::new();
        let mut pending = vec![node];
        while let Some(current) = pending.pop() {
            if current < self.item_count {
                members.push(current);
                continue;
            }
            let merge = self.merges[current - self.item_count];
            pending.push(merge.right);
            pending.push(merge.left);
        }
        members.sort_unstable();
        members
    }
}

fn ensure_not_cancelled(cancelled: &AtomicBool) -> anyhow::Result<()> {
    anyhow::ensure!(
        !cancelled.load(AtomicOrdering::Relaxed),
        "Chunk topic operation cancelled"
    );
    Ok(())
}

fn normalize(vector: &[f32]) -> Vec<f32> {
    let norm = vector.iter().map(|value| value * value).sum::<f32>().sqrt();
    if norm <= f32::EPSILON {
        return vec![0.0; vector.len()];
    }
    vector.iter().map(|value| value / norm).collect()
}

fn dot(left: &[f32], right: &[f32]) -> f32 {
    left.iter()
        .zip(right)
        .map(|(left, right)| left * right)
        .sum()
}

fn ward_merge_cost(
    left_centroid: &[f32],
    left_size: usize,
    right_centroid: &[f32],
    right_size: usize,
) -> f32 {
    let squared_distance = left_centroid
        .iter()
        .zip(right_centroid)
        .map(|(left, right)| {
            let difference = left - right;
            difference * difference
        })
        .sum::<f32>();
    left_size as f32 * right_size as f32 / (left_size + right_size) as f32 * squared_distance
}

/// Ward's singleton coefficient is one half, so its merge cost can reuse the
/// dot product already computed for the similarity matrix instead of walking
/// every embedding dimension a second time. Keeping the squared norms makes
/// this exact for the zero vectors accepted by `normalize` too; `1 - cosine`
/// alone would incorrectly assign a positive cost to two zero vectors.
fn singleton_ward_merge_cost(
    left_squared_norm: f32,
    right_squared_norm: f32,
    similarity: f32,
) -> f32 {
    (0.5 * (left_squared_norm + right_squared_norm) - similarity).max(0.0)
}

fn silhouette_score(
    partition: &[Vec<usize>],
    similarities: &[Vec<f32>],
    cancelled: &AtomicBool,
) -> anyhow::Result<f32> {
    let item_count = similarities.len();
    let mut cluster_for_item = vec![0usize; item_count];
    for (cluster_index, cluster) in partition.iter().enumerate() {
        for &item in cluster {
            cluster_for_item[item] = cluster_index;
        }
    }

    let mut total = 0.0;
    for item in 0..item_count {
        ensure_not_cancelled(cancelled)?;
        let own = &partition[cluster_for_item[item]];
        if own.len() <= 1 {
            continue;
        }
        let within = own
            .iter()
            .filter(|&&other| other != item)
            .map(|&other| 1.0 - similarities[item][other])
            .sum::<f32>()
            / (own.len() - 1) as f32;
        let nearest_other = partition
            .iter()
            .enumerate()
            .filter(|(index, _)| *index != cluster_for_item[item])
            .map(|(_, cluster)| {
                cluster
                    .iter()
                    .map(|&other| 1.0 - similarities[item][other])
                    .sum::<f32>()
                    / cluster.len() as f32
            })
            .fold(f32::INFINITY, f32::min);
        let denominator = within.max(nearest_other);
        if denominator > f32::EPSILON && nearest_other.is_finite() {
            total += (nearest_other - within) / denominator;
        }
    }
    Ok(total / item_count as f32)
}

fn medoid(
    group: &[usize],
    similarities: &[Vec<f32>],
    cancelled: &AtomicBool,
) -> anyhow::Result<usize> {
    let mut best = group[0];
    let mut best_score = f32::NEG_INFINITY;
    for &candidate in group {
        ensure_not_cancelled(cancelled)?;
        let score = group
            .iter()
            .filter(|&&other| other != candidate)
            .map(|&other| similarities[candidate][other])
            .sum::<f32>();
        if score.total_cmp(&best_score).is_gt()
            || (score.total_cmp(&best_score).is_eq() && candidate < best)
        {
            best = candidate;
            best_score = score;
        }
    }
    Ok(best)
}

fn average_pair_similarity(
    group: &[usize],
    similarities: &[Vec<f32>],
    cancelled: &AtomicBool,
) -> anyhow::Result<f32> {
    let mut total = 0.0;
    let mut pairs = 0usize;
    for (offset, &left) in group.iter().enumerate() {
        ensure_not_cancelled(cancelled)?;
        for &right in &group[(offset + 1)..] {
            total += similarities[left][right];
            pairs += 1;
        }
    }
    if pairs == 0 {
        Ok(1.0)
    } else {
        Ok(total / pairs as f32)
    }
}

fn integer_sqrt_ceil(value: usize) -> usize {
    let mut root = 0usize;
    while root.saturating_mul(root) < value {
        root += 1;
    }
    root
}

fn cluster_count_for_granularity(
    item_count: usize,
    granularity: BookmarkClusterGranularity,
) -> usize {
    debug_assert!(item_count >= 3);
    // Balanced groups average at least three items for small collections. The
    // adjustable ceiling permits finer two-item themes when explicitly asked.
    let balanced = integer_sqrt_ceil(item_count)
        .min((item_count / 3).max(2))
        .max(2);
    let requested = match granularity {
        BookmarkClusterGranularity::MuchFewer => ceil_ratio(balanced, 2),
        BookmarkClusterGranularity::Fewer => ceil_ratio(balanced.saturating_mul(3), 4),
        BookmarkClusterGranularity::Balanced => balanced,
        BookmarkClusterGranularity::More => ceil_ratio(balanced.saturating_mul(3), 2),
        BookmarkClusterGranularity::MuchMore => balanced.saturating_mul(2),
    };
    let maximum = (item_count / 2).max(2).min(item_count - 1);
    requested.clamp(2, maximum)
}

fn ceil_ratio(numerator: usize, denominator: usize) -> usize {
    numerator / denominator + usize::from(!numerator.is_multiple_of(denominator))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn next_test_value(state: &mut u64) -> f32 {
        *state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((*state >> 32) as f32 / u32::MAX as f32) * 2.0 - 1.0
    }

    fn test_vector(state: &mut u64, dimension: usize) -> Vec<f32> {
        (0..dimension).map(|_| next_test_value(state)).collect()
    }

    fn normalized_test_vector(vector: Vec<f32>) -> Vec<f32> {
        normalize(&vector)
    }

    fn cluster_balanced(vectors: &[Vec<f32>]) -> anyhow::Result<EmbeddingClusterResult> {
        cluster_embeddings(vectors, BookmarkClusterGranularity::Balanced)
    }

    #[test]
    fn singleton_cost_reuses_similarity_without_changing_zero_vector_behavior() {
        let vectors = [
            normalize(&[3.0, 4.0]),
            normalize(&[-2.0, 5.0]),
            normalize(&[0.0, 0.0]),
        ];

        for left in 0..vectors.len() {
            for right in (left + 1)..vectors.len() {
                let similarity = dot(&vectors[left], &vectors[right]).clamp(-1.0, 1.0);
                let reused = singleton_ward_merge_cost(
                    dot(&vectors[left], &vectors[left]),
                    dot(&vectors[right], &vectors[right]),
                    similarity,
                );
                let direct = ward_merge_cost(&vectors[left], 1, &vectors[right], 1);
                assert!((reused - direct).abs() <= 1e-6, "{reused} != {direct}");
            }
        }

        assert_eq!(singleton_ward_merge_cost(0.0, 0.0, 0.0), 0.0);
    }

    #[test]
    fn separates_clear_groups_and_selects_medoids() {
        let result = cluster_balanced(&[
            vec![1.0, 0.0],
            vec![0.99, 0.05],
            vec![0.95, -0.05],
            vec![0.0, 1.0],
            vec![0.05, 0.99],
            vec![-0.05, 0.95],
        ])
        .unwrap();

        assert_eq!(result.clusters.len(), 2);
        assert_eq!(result.clusters[0].item_indices, vec![0, 1, 2]);
        assert_eq!(result.clusters[1].item_indices, vec![3, 4, 5]);
        assert!(result.unclustered_indices.is_empty());
        assert_eq!(result.clusters[0].representative_index, 0);
        assert_eq!(result.clusters[1].representative_index, 3);
    }

    #[test]
    fn avoids_an_outlier_pair_creating_one_dominant_cluster() {
        let mut state = 3u64;
        let dimension = 12;
        let centers: Vec<Vec<f32>> = (0..8)
            .map(|_| normalized_test_vector(test_vector(&mut state, dimension)))
            .collect();
        let mut vectors = Vec::new();

        for (index, center) in centers.iter().enumerate() {
            let item_count = if index < 6 { 9 } else { 7 };
            for _ in 0..item_count {
                let noise = test_vector(&mut state, dimension);
                vectors.push(
                    center
                        .iter()
                        .zip(noise)
                        .map(|(value, noise)| value + noise)
                        .collect(),
                );
            }
        }

        let outlier_center = normalized_test_vector(test_vector(&mut state, dimension));
        for _ in 0..2 {
            let noise = test_vector(&mut state, dimension);
            vectors.push(
                outlier_center
                    .iter()
                    .zip(noise)
                    .map(|(value, noise)| value * 3.0 + noise * 0.02)
                    .collect(),
            );
        }

        let result = cluster_balanced(&vectors).unwrap();
        let largest_cluster = result
            .clusters
            .iter()
            .map(|cluster| cluster.item_indices.len())
            .max()
            .unwrap();

        assert_eq!(vectors.len(), 70);
        assert_eq!(result.clusters.len(), 9);
        assert!(largest_cluster <= 24);
        assert_eq!(cluster_balanced(&vectors).unwrap(), result);
    }

    #[test]
    fn granularity_scales_from_a_stable_balanced_default() {
        assert_eq!(
            cluster_count_for_granularity(71, BookmarkClusterGranularity::MuchFewer),
            5
        );
        assert_eq!(
            cluster_count_for_granularity(71, BookmarkClusterGranularity::Fewer),
            7
        );
        assert_eq!(
            cluster_count_for_granularity(71, BookmarkClusterGranularity::Balanced),
            9
        );
        assert_eq!(
            cluster_count_for_granularity(71, BookmarkClusterGranularity::More),
            14
        );
        assert_eq!(
            cluster_count_for_granularity(71, BookmarkClusterGranularity::MuchMore),
            18
        );
        assert_eq!(
            cluster_count_for_granularity(6, BookmarkClusterGranularity::Balanced),
            2
        );
    }

    #[test]
    fn one_tree_can_be_recut_in_any_granularity_order() {
        let vectors = vec![
            vec![1.0, 0.0, 0.0],
            vec![0.98, 0.04, 0.0],
            vec![0.95, -0.03, 0.0],
            vec![0.0, 1.0, 0.0],
            vec![0.03, 0.97, 0.0],
            vec![-0.04, 0.96, 0.0],
            vec![0.0, 0.0, 1.0],
            vec![0.02, 0.0, 0.99],
            vec![-0.03, 0.0, 0.97],
        ];
        let tree = WardTree::build(&vectors).unwrap();
        assert_eq!(tree.merges.len(), vectors.len() - 1);

        let more = tree.cut(BookmarkClusterGranularity::MuchMore).unwrap();
        let fewer = tree.cut(BookmarkClusterGranularity::MuchFewer).unwrap();
        assert_eq!(
            tree.cut(BookmarkClusterGranularity::MuchMore).unwrap(),
            more
        );
        assert_eq!(
            tree.cut(BookmarkClusterGranularity::MuchFewer).unwrap(),
            fewer
        );
        assert_eq!(
            cluster_embeddings(&vectors, BookmarkClusterGranularity::MuchMore).unwrap(),
            more
        );
        assert_eq!(
            cluster_embeddings(&vectors, BookmarkClusterGranularity::MuchFewer).unwrap(),
            fewer
        );
    }

    #[test]
    fn leaves_too_few_items_unclustered() {
        let result = cluster_balanced(&[vec![1.0], vec![1.0]]).unwrap();
        assert!(result.clusters.is_empty());
        assert_eq!(result.unclustered_indices, vec![0, 1]);
    }

    #[test]
    fn cancellable_build_and_cut_stop_before_work() {
        let vectors = vec![vec![1.0, 0.0], vec![0.9, 0.1], vec![0.0, 1.0]];
        let cancelled = AtomicBool::new(true);
        assert!(WardTree::build_with_cancel(&vectors, &cancelled)
            .unwrap_err()
            .to_string()
            .contains("cancelled"));

        let tree = WardTree::build(&vectors).unwrap();
        assert!(tree
            .cut_with_cancel(BookmarkClusterGranularity::Balanced, &cancelled)
            .unwrap_err()
            .to_string()
            .contains("cancelled"));
    }

    #[test]
    fn leaves_indistinguishable_items_unclustered() {
        let result = cluster_balanced(&[
            vec![1.0, 0.0],
            vec![1.0, 0.0],
            vec![1.0, 0.0],
            vec![1.0, 0.0],
        ])
        .unwrap();
        assert!(result.clusters.is_empty());
        assert_eq!(result.unclustered_indices, vec![0, 1, 2, 3]);
    }

    #[test]
    fn rejects_mismatched_dimensions() {
        assert!(cluster_balanced(&[vec![1.0], vec![1.0, 2.0], vec![3.0]]).is_err());
    }
}
