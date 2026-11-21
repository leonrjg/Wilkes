//! Where the documents of one folder would sit better: the scoring behind
//! "Tidy up".
//!
//! A destination folder is known by two things: the documents already in it,
//! and its name. Neither is enough alone. The documents say what a folder is
//! really used for, which its name may not (a "Content analysis" folder full
//! of recommender papers). The name is all an empty folder has, and an empty
//! folder made for the purpose is exactly where a tidy-up should put things.
//!
//! The two are not on one scale. A document sits near the centroid of other
//! documents of its kind — every paper shares the register of papers — and far
//! from a two-word name, so a raw cosine to a centroid is around 0.7 where the
//! cosine to a name is around 0.3, for the same goodness of fit. Each signal is
//! therefore standardised across the destinations for the document at hand: it
//! is how much nearer this folder is than the others are, by that measure. A
//! folder's score is the mean of the standardised signals it has, so an empty
//! folder competes on its name alone rather than being scored as a poor fit.

/// One destination, as the scoring sees it.
#[derive(Clone, Debug)]
pub struct FolderProfile {
    /// The folder's name embedded in the query role: it describes what to
    /// look for, as a search query does.
    pub name: Vec<f32>,
    /// The normalised mean of the documents already in the folder, or `None`
    /// when none of them is in the semantic index.
    pub content: Option<Vec<f32>>,
}

/// One destination's fit for a document, by index into the profiles given.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placement {
    pub folder: usize,
    pub score: f32,
}

/// A suggestion whose best destination leads the next by less than this many
/// standard deviations is a close call: the document fits two places about
/// equally, and moving it is the user's decision rather than the tidy-up's.
pub const CLOSE_CALL_MARGIN: f32 = 0.25;

/// The normalised mean of `documents`, each itself normalised first so a
/// document is one vote however long it is. `None` for no documents.
pub fn folder_centroid<'a>(documents: impl IntoIterator<Item = &'a [f32]>) -> Option<Vec<f32>> {
    let mut sum: Option<Vec<f32>> = None;
    for document in documents {
        let normalized = normalized(document);
        match sum.as_mut() {
            Some(total) => {
                for (total, value) in total.iter_mut().zip(normalized) {
                    *total += value;
                }
            }
            None => sum = Some(normalized),
        }
    }
    sum.map(|total| normalized(&total))
}

/// Every destination ranked for `document`, best first. Ties keep the order
/// the profiles were given in, so a ranking is reproducible.
pub fn rank_folders(document: &[f32], folders: &[FolderProfile]) -> Vec<Placement> {
    let names = standardized(
        &folders
            .iter()
            .map(|folder| cosine(document, &folder.name))
            .collect::<Vec<_>>(),
    );

    // Only folders that hold indexed documents have a content signal, and it
    // is standardised among them. One such folder has nothing to be compared
    // with, so its content says nothing about it relative to the rest.
    let with_content: Vec<usize> = folders
        .iter()
        .enumerate()
        .filter_map(|(index, folder)| folder.content.as_ref().map(|_| index))
        .collect();
    let mut content: Vec<Option<f32>> = vec![None; folders.len()];
    if with_content.len() > 1 {
        let raw: Vec<f32> = with_content
            .iter()
            .map(|&index| {
                let centroid = folders[index].content.as_deref().unwrap_or_default();
                cosine(document, centroid)
            })
            .collect();
        for (&index, z) in with_content.iter().zip(standardized(&raw)) {
            content[index] = Some(z);
        }
    }

    let mut placements: Vec<Placement> = names
        .iter()
        .zip(&content)
        .enumerate()
        .map(|(folder, (&name, content))| Placement {
            folder,
            score: match content {
                Some(content) => (name + content) / 2.0,
                None => name,
            },
        })
        .collect();
    placements.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.folder.cmp(&b.folder)));
    placements
}

/// Whether the best of `ranking` leads the runner-up by less than
/// [`CLOSE_CALL_MARGIN`]. A single destination is never a close call: there is
/// nothing for it to be close to.
pub fn is_close_call(ranking: &[Placement]) -> bool {
    match ranking {
        [best, runner_up, ..] => best.score - runner_up.score < CLOSE_CALL_MARGIN,
        _ => false,
    }
}

/// `values` as standard scores. All zero when they do not vary, which is the
/// honest answer: no value stands out from the others.
fn standardized(values: &[f32]) -> Vec<f32> {
    if values.is_empty() {
        return Vec::new();
    }
    let count = values.len() as f32;
    let mean = values.iter().sum::<f32>() / count;
    let variance = values
        .iter()
        .map(|value| (value - mean).powi(2))
        .sum::<f32>()
        / count;
    let deviation = variance.sqrt();
    if deviation <= f32::EPSILON {
        return vec![0.0; values.len()];
    }
    values
        .iter()
        .map(|value| (value - mean) / deviation)
        .collect()
}

fn normalized(vector: &[f32]) -> Vec<f32> {
    let norm = vector.iter().map(|value| value * value).sum::<f32>().sqrt();
    if norm == 0.0 {
        return vec![0.0; vector.len()];
    }
    vector.iter().map(|value| value / norm).collect()
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(name: [f32; 3], content: Option<[f32; 3]>) -> FolderProfile {
        FolderProfile {
            name: name.to_vec(),
            content: content.map(|vector| vector.to_vec()),
        }
    }

    #[test]
    fn an_empty_folder_wins_on_its_name_when_its_name_is_the_clear_fit() {
        // A recommender paper, a broad folder whose documents it resembles
        // somewhat, a legal folder, and an empty "Recommendation systems".
        let document = [0.9, 0.4, 0.1];
        let folders = [
            profile([0.3, 0.9, 0.1], Some([0.7, 0.7, 0.1])),
            profile([0.0, 0.1, 1.0], Some([0.1, 0.2, 1.0])),
            profile([1.0, 0.1, 0.0], None),
        ];

        let ranking = rank_folders(&document, &folders);

        assert_eq!(ranking[0].folder, 2);
        assert_eq!(ranking.len(), 3);
    }

    #[test]
    fn documents_in_a_folder_outweigh_a_name_that_says_little() {
        // The second folder's name is nearer, but its documents are about
        // something else; the first folder's documents are this document's kind.
        let document = [1.0, 0.0, 0.0];
        let folders = [
            profile([0.2, 1.0, 0.0], Some([1.0, 0.05, 0.0])),
            profile([0.5, 0.8, 0.0], Some([0.0, 0.0, 1.0])),
            profile([0.0, 0.0, 1.0], Some([0.0, 1.0, 0.0])),
        ];

        let ranking = rank_folders(&document, &folders);

        assert_eq!(ranking[0].folder, 0);
    }

    #[test]
    fn a_lone_content_signal_is_not_compared_with_nothing() {
        // Only one folder has indexed documents. Its content cannot be judged
        // against the others', so both are ranked by name alone.
        let document = [1.0, 0.0, 0.0];
        let folders = [
            profile([0.0, 1.0, 0.0], Some([1.0, 0.0, 0.0])),
            profile([1.0, 0.0, 0.0], None),
        ];

        let ranking = rank_folders(&document, &folders);

        assert_eq!(ranking[0].folder, 1);
    }

    #[test]
    fn identical_folders_tie_in_the_order_given_and_are_a_close_call() {
        let document = [1.0, 0.0, 0.0];
        let folders = [
            profile([0.5, 0.5, 0.0], None),
            profile([0.5, 0.5, 0.0], None),
        ];

        let ranking = rank_folders(&document, &folders);

        assert_eq!(
            ranking
                .iter()
                .map(|placement| placement.folder)
                .collect::<Vec<_>>(),
            vec![0, 1]
        );
        assert!(is_close_call(&ranking));
    }

    #[test]
    fn a_single_destination_is_never_a_close_call() {
        let ranking = rank_folders(&[1.0, 0.0, 0.0], &[profile([0.0, 1.0, 0.0], None)]);
        assert_eq!(
            ranking,
            vec![Placement {
                folder: 0,
                score: 0.0
            }]
        );
        assert!(!is_close_call(&ranking));
    }

    #[test]
    fn a_clear_lead_is_not_a_close_call() {
        let document = [1.0, 0.0, 0.0];
        let folders = [
            profile([1.0, 0.0, 0.0], None),
            profile([0.0, 1.0, 0.0], None),
            profile([0.0, 0.0, 1.0], None),
        ];

        assert!(!is_close_call(&rank_folders(&document, &folders)));
    }

    #[test]
    fn a_folder_centroid_counts_each_document_once_whatever_its_length() {
        let long = [10.0, 0.0];
        let short = [0.0, 0.1];
        let centroid = folder_centroid([long.as_slice(), short.as_slice()]).unwrap();
        assert!((centroid[0] - centroid[1]).abs() < 1e-6);
        assert!((centroid[0].hypot(centroid[1]) - 1.0).abs() < 1e-6);
        assert!(folder_centroid(std::iter::empty()).is_none());
    }
}
