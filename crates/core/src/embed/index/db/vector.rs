//! Vector encoding and arithmetic shared by building and querying.

pub(super) fn f32_slice_to_bytes(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|f| f.to_le_bytes()).collect()
}

pub(super) fn f32_slice_from_bytes(bytes: &[u8]) -> anyhow::Result<Vec<f32>> {
    anyhow::ensure!(
        bytes.len().is_multiple_of(std::mem::size_of::<f32>()),
        "Invalid embedding byte length: {}",
        bytes.len()
    );
    Ok(bytes
        .chunks_exact(std::mem::size_of::<f32>())
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect())
}

pub(super) fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let (mut dot, mut norm_a, mut norm_b) = (0.0f32, 0.0f32, 0.0f32);
    for (left, right) in a.iter().zip(b) {
        dot += left * right;
        norm_a += left * left;
        norm_b += right * right;
    }
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    dot / (norm_a.sqrt() * norm_b.sqrt())
}

pub(super) fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

pub(super) fn normalized_vector(v: &[f32]) -> Vec<f32> {
    let norm = v.iter().map(|value| value * value).sum::<f32>().sqrt();
    if norm == 0.0 {
        return vec![0.0; v.len()];
    }
    v.iter().map(|value| value / norm).collect()
}

/// A document's normalised chunk vectors, summed as they are read.
pub(super) struct DocumentVectorSum {
    pub(super) sum: Vec<f32>,
    pub(super) chunks: usize,
}

impl DocumentVectorSum {
    pub(super) fn centroid(&self) -> Vec<f32> {
        centroid(&self.sum, self.chunks)
    }
}

pub(super) fn centroid(sum: &[f32], count: usize) -> Vec<f32> {
    if count == 0 {
        return vec![0.0; sum.len()];
    }
    let count = count as f32;
    sum.iter().map(|value| value / count).collect()
}
