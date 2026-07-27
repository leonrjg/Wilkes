use std::cmp::Ordering;
use std::collections::BinaryHeap;

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
/// single catch-all cluster. Candidate cuts are scored with cosine-distance
/// silhouette quality. Singleton groups in the winning cut are returned as
/// unclustered items.
pub fn cluster_embeddings(vectors: &[Vec<f32>]) -> anyhow::Result<EmbeddingClusterResult> {
    let count = vectors.len();
    if count < 3 {
        return Ok(EmbeddingClusterResult {
            clusters: Vec::new(),
            unclustered_indices: (0..count).collect(),
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

    let normalized: Vec<Vec<f32>> = vectors.iter().map(|vector| normalize(vector)).collect();
    let mut item_similarities = vec![vec![0.0; count]; count];
    for index in 0..count {
        item_similarities[index][index] = 1.0;
        for other in (index + 1)..count {
            let similarity = dot(&normalized[index], &normalized[other]).clamp(-1.0, 1.0);
            item_similarities[index][other] = similarity;
            item_similarities[other][index] = similarity;
        }
    }

    let capacity = count * 2 - 1;
    let mut members: Vec<Vec<usize>> = vec![Vec::new(); capacity];
    let mut centroids: Vec<Vec<f32>> = vec![Vec::new(); capacity];
    let mut sizes = vec![0usize; capacity];
    let mut active = vec![false; capacity];
    let mut heap = BinaryHeap::new();

    for index in 0..count {
        members[index].push(index);
        centroids[index] = normalized[index].clone();
        sizes[index] = 1;
        active[index] = true;
        for other in 0..index {
            heap.push(WardEdge {
                cost: ward_merge_cost(
                    &centroids[other],
                    sizes[other],
                    &centroids[index],
                    sizes[index],
                ),
                left: other,
                right: index,
            });
        }
    }

    let max_candidate_clusters = integer_sqrt_ceil(count).max(2).min(count - 1);
    let mut active_count = count;
    let mut next_cluster = count;
    let mut best: Option<(f32, Vec<Vec<usize>>)> = None;

    while active_count > 1 {
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
        members[merged] = members[edge.left]
            .iter()
            .chain(&members[edge.right])
            .copied()
            .collect();
        members[merged].sort_unstable();

        active[edge.left] = false;
        active[edge.right] = false;
        active[merged] = true;
        active_count -= 1;

        for other in 0..merged {
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

        if (2..=max_candidate_clusters).contains(&active_count) {
            let mut partition: Vec<Vec<usize>> = active
                .iter()
                .enumerate()
                .filter(|(_, is_active)| **is_active)
                .map(|(index, _)| members[index].clone())
                .collect();
            partition.sort_by_key(|group| group[0]);
            let score = silhouette_score(&partition, &item_similarities);
            let should_replace = match &best {
                None => true,
                Some((best_score, best_partition)) => {
                    score > *best_score + f32::EPSILON
                        || ((score - *best_score).abs() <= f32::EPSILON
                            && partition.len() < best_partition.len())
                }
            };
            if should_replace {
                best = Some((score, partition));
            }
        }
    }

    let Some((_, partition)) = best.filter(|(score, _)| *score > f32::EPSILON) else {
        return Ok(EmbeddingClusterResult {
            clusters: Vec::new(),
            unclustered_indices: (0..count).collect(),
        });
    };

    let mut result = EmbeddingClusterResult::default();
    for mut group in partition {
        group.sort_unstable();
        if group.len() < 2 {
            result.unclustered_indices.extend(group);
            continue;
        }
        let representative_index = medoid(&group, &item_similarities);
        let cohesion = average_pair_similarity(&group, &item_similarities);
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

fn silhouette_score(partition: &[Vec<usize>], similarities: &[Vec<f32>]) -> f32 {
    let item_count = similarities.len();
    let mut cluster_for_item = vec![0usize; item_count];
    for (cluster_index, cluster) in partition.iter().enumerate() {
        for &item in cluster {
            cluster_for_item[item] = cluster_index;
        }
    }

    let mut total = 0.0;
    for item in 0..item_count {
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
    total / item_count as f32
}

fn medoid(group: &[usize], similarities: &[Vec<f32>]) -> usize {
    group
        .iter()
        .copied()
        .max_by(|&left, &right| {
            let left_score: f32 = group
                .iter()
                .filter(|&&other| other != left)
                .map(|&other| similarities[left][other])
                .sum();
            let right_score: f32 = group
                .iter()
                .filter(|&&other| other != right)
                .map(|&other| similarities[right][other])
                .sum();
            left_score
                .total_cmp(&right_score)
                .then_with(|| right.cmp(&left))
        })
        .unwrap_or(group[0])
}

fn average_pair_similarity(group: &[usize], similarities: &[Vec<f32>]) -> f32 {
    let mut total = 0.0;
    let mut pairs = 0usize;
    for (offset, &left) in group.iter().enumerate() {
        for &right in &group[(offset + 1)..] {
            total += similarities[left][right];
            pairs += 1;
        }
    }
    if pairs == 0 {
        1.0
    } else {
        total / pairs as f32
    }
}

fn integer_sqrt_ceil(value: usize) -> usize {
    let mut root = 0usize;
    while root.saturating_mul(root) < value {
        root += 1;
    }
    root
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

    #[test]
    fn separates_clear_groups_and_selects_medoids() {
        let result = cluster_embeddings(&[
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

        let result = cluster_embeddings(&vectors).unwrap();
        let largest_cluster = result
            .clusters
            .iter()
            .map(|cluster| cluster.item_indices.len())
            .max()
            .unwrap();

        assert_eq!(vectors.len(), 70);
        assert!(result.clusters.len() >= 6);
        assert!(largest_cluster <= 24);
        assert_eq!(cluster_embeddings(&vectors).unwrap(), result);
    }

    #[test]
    fn leaves_too_few_items_unclustered() {
        let result = cluster_embeddings(&[vec![1.0], vec![1.0]]).unwrap();
        assert!(result.clusters.is_empty());
        assert_eq!(result.unclustered_indices, vec![0, 1]);
    }

    #[test]
    fn leaves_indistinguishable_items_unclustered() {
        let result = cluster_embeddings(&[
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
        assert!(cluster_embeddings(&[vec![1.0], vec![1.0, 2.0], vec![3.0]]).is_err());
    }
}
