//! Chunk references: resolution, figures, neighbours, accumulation, similarity
//! and search.

use super::*;

/// The most neighbours sqlite-vec's nearest-neighbour scan returns at once.
/// `managed_chunk_search` widens up to this while closing a tie at `top_k`.
pub(super) const MANAGED_SEARCH_MAX_NEIGHBOURS: usize = 4096;

impl SemanticIndex {
    /// Resolve opaque refs to local execution rowids. All refs must resolve;
    /// rowids never leave this method's callers in the managed surface.
    pub fn resolve_chunk_refs(
        &self,
        refs: &HashSet<ChunkRef>,
    ) -> anyhow::Result<HashMap<ChunkRef, i64>> {
        const REFS_PER_QUERY: usize = 400;
        let refs: Vec<ChunkRef> = refs.iter().cloned().collect();
        let mut found = HashMap::with_capacity(refs.len());
        for batch in refs.chunks(REFS_PER_QUERY) {
            let placeholders = vec!["?"; batch.len()].join(",");
            let mut stmt = self.conn.prepare(&format!(
                "SELECT chunk_ref, id FROM chunks
                 WHERE chunk_ref IN ({placeholders})"
            ))?;
            let rows = stmt.query_map(
                params_from_iter(batch.iter().map(ChunkRef::as_str)),
                |row| Ok((ChunkRef(row.get(0)?), row.get::<_, i64>(1)?)),
            )?;
            for row in rows {
                let (stable_ref, rowid) = row?;
                found.insert(stable_ref, rowid);
            }
        }
        if found.len() != refs.len() {
            let mut missing: Vec<&str> = refs
                .iter()
                .filter(|stable_ref| !found.contains_key(*stable_ref))
                .map(ChunkRef::as_str)
                .collect();
            missing.sort_unstable();
            consumer_bail!(
                ConsumerErrorCode::ChunkRefNotFound,
                "{} reference(s) do not belong to this corpus: {}",
                missing.len(),
                missing.into_iter().take(10).collect::<Vec<_>>().join(", "),
            );
        }
        Ok(found)
    }
    /// The figures each of these passages bears on, strongest relation first
    /// and nearest first within a relation.
    ///
    /// One retained extraction is read per document rather than per passage:
    /// the passages of one weave are usually the same handful of books, and
    /// the linking itself is arithmetic over ranges already in hand.
    ///
    /// `window` is bytes of slack outside the passage — zero, the default the
    /// pipeline ships, admits only the figures a passage contains.
    pub fn figures_for_chunk_refs(
        &self,
        refs: &[ChunkRef],
        window: usize,
    ) -> anyhow::Result<Vec<ChunkFigures>> {
        let wanted: HashSet<ChunkRef> = refs.iter().cloned().collect();
        self.resolve_chunk_refs(&wanted)?;

        const REFS_PER_QUERY: usize = 300;
        let unique: Vec<ChunkRef> = wanted.into_iter().collect();
        let mut located: HashMap<ChunkRef, (i64, ByteRange)> = HashMap::with_capacity(unique.len());
        for batch in unique.chunks(REFS_PER_QUERY) {
            let placeholders = vec!["?"; batch.len()].join(",");
            let mut stmt = self.conn.prepare(&format!(
                "SELECT chunk_ref, file_id, byte_start, byte_end
                 FROM chunks WHERE chunk_ref IN ({placeholders})"
            ))?;
            let rows = stmt.query_map(
                params_from_iter(batch.iter().map(ChunkRef::as_str)),
                |row| {
                    Ok((
                        ChunkRef(row.get(0)?),
                        row.get::<_, i64>(1)?,
                        ByteRange {
                            start: row.get::<_, i64>(2)? as usize,
                            end: row.get::<_, i64>(3)? as usize,
                        },
                    ))
                },
            )?;
            for row in rows {
                let (chunk_ref, file_id, range) = row?;
                located.insert(chunk_ref, (file_id, range));
            }
        }

        let mut retained_by_file: HashMap<i64, RetainedExtraction> = HashMap::new();
        let mut out = Vec::with_capacity(refs.len());
        // In the order asked, and once per ref even if the caller repeated one.
        for chunk_ref in refs {
            let Some((file_id, range)) = located.get(chunk_ref) else {
                continue;
            };
            let retained = match retained_by_file.get(file_id) {
                Some(retained) => retained,
                None => {
                    let loaded = self.retained_extraction_for_file(*file_id)?;
                    retained_by_file.entry(*file_id).or_insert(loaded)
                }
            };
            let mut figures: Vec<LinkedFigure> = retained
                .images
                .iter()
                .flatten()
                .filter_map(|image| {
                    figure::link(image, range, window).map(|link| LinkedFigure {
                        image: image.clone(),
                        link,
                    })
                })
                .collect();
            // Strongest relation first, nearest first within one, then by id
            // so a tie is not decided by hash order.
            figures.sort_by(|a, b| {
                a.link
                    .relation
                    .cmp(&b.link.relation)
                    .then(a.link.byte_distance.cmp(&b.link.byte_distance))
                    .then(a.image.id.cmp(&b.image.id))
            });
            out.push(ChunkFigures {
                chunk_ref: chunk_ref.clone(),
                inventory_known: retained.images.is_some(),
                figures,
            });
        }
        Ok(out)
    }
    /// One document's image inventory, in reading order, with no passage in
    /// the question. `None` when the index does not hold this file at all —
    /// which is a different answer from a file whose inventory is unknown.
    pub fn figures_for_path(
        &self,
        path: &Path,
    ) -> anyhow::Result<Option<(bool, Vec<RetainedImage>)>> {
        let key = self.path_key_for_known_path(path);
        let file_id: Option<i64> = self
            .conn
            .query_row(
                "SELECT id FROM files WHERE file_path = ?1",
                params![key.to_string_lossy()],
                |row| row.get(0),
            )
            .optional()?;
        let Some(file_id) = file_id else {
            return Ok(None);
        };
        let retained = self.retained_extraction_for_file(file_id)?;
        Ok(Some((
            retained.images.is_some(),
            retained.images.unwrap_or_default(),
        )))
    }
    /// Where to find the pixels of one figure of one document.
    ///
    /// The document is named either by path or by any passage of it, because
    /// a managed consumer holds chunk refs and never a path. Both resolve to
    /// the same file row, and the answer says where its bytes are rather than
    /// reading them: this layer holds the index, not the data directory the
    /// snapshot sits under.
    pub fn figure_source(
        &self,
        document: FigureDocument<'_>,
        area_id: &str,
    ) -> anyhow::Result<Option<FigureSource>> {
        let file_id = match document {
            FigureDocument::Path(path) => {
                let key = self.path_key_for_known_path(path);
                self.conn
                    .query_row(
                        "SELECT id FROM files WHERE file_path = ?1",
                        params![key.to_string_lossy()],
                        |row| row.get::<_, i64>(0),
                    )
                    .optional()?
            }
            FigureDocument::Chunk(chunk_ref) => self
                .conn
                .query_row(
                    "SELECT file_id FROM chunks WHERE chunk_ref = ?1",
                    params![chunk_ref.as_str()],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?,
        };
        let Some(file_id) = file_id else {
            return Ok(None);
        };

        let Some(image) = self
            .retained_extraction_for_file(file_id)?
            .images
            .unwrap_or_default()
            .into_iter()
            .find(|image| image.id == area_id)
        else {
            return Ok(None);
        };

        let (file_path, managed_snapshot_relative_path) = self.conn.query_row(
            "SELECT file_path, managed_snapshot_relative_path FROM files WHERE id = ?1",
            params![file_id],
            |row| {
                Ok((
                    PathBuf::from(row.get::<_, String>(0)?),
                    row.get::<_, Option<String>>(1)?,
                ))
            },
        )?;
        Ok(Some(FigureSource {
            managed_snapshot_relative_path,
            file_path,
            image,
        }))
    }
    /// Where the retained bytes of one imported document are, by snapshot id.
    ///
    /// The retained copy only — never `file_path`, which is where the document
    /// came *from*. That path belongs to whichever machine indexed it and may
    /// have been edited, moved or deleted since; the snapshot is immutable and
    /// is what the rendition was built from, so it is the only answer that
    /// still describes the passages a consumer holds.
    ///
    /// A snapshot id is `identity::snapshot_id(source_sha256)`, so the same
    /// bytes imported under two names are one snapshot with two file rows.
    /// Any of them answers: what is being asked for is the bytes, and they are
    /// the same bytes.
    pub fn managed_snapshot_source(
        &self,
        snapshot_id: &str,
    ) -> anyhow::Result<Option<ManagedSnapshotSource>> {
        Ok(self
            .conn
            .query_row(
                "SELECT managed_snapshot_relative_path, source_sha256 FROM files
                  WHERE snapshot_id = ?1
                    AND managed_snapshot_relative_path IS NOT NULL
                    AND source_sha256 IS NOT NULL
                  LIMIT 1",
                params![snapshot_id],
                |row| {
                    Ok(ManagedSnapshotSource {
                        managed_snapshot_relative_path: row.get(0)?,
                        source_sha256: row.get(1)?,
                    })
                },
            )
            .optional()?)
    }
    pub fn managed_chunks_for_refs(
        &self,
        refs: &[ChunkRef],
    ) -> anyhow::Result<Vec<ManagedChunkData>> {
        let wanted: HashSet<ChunkRef> = refs.iter().cloned().collect();
        self.resolve_chunk_refs(&wanted)?;
        const REFS_PER_QUERY: usize = 300;
        let unique: Vec<ChunkRef> = wanted.into_iter().collect();
        let mut found = HashMap::with_capacity(unique.len());
        for batch in unique.chunks(REFS_PER_QUERY) {
            let placeholders = vec!["?"; batch.len()].join(",");
            let mut stmt = self.conn.prepare(&format!(
                "SELECT {MANAGED_CHUNK_COLUMNS}
                 FROM chunks WHERE chunk_ref IN ({placeholders})"
            ))?;
            let rows = stmt.query_map(
                params_from_iter(batch.iter().map(ChunkRef::as_str)),
                managed_chunk_from_row,
            )?;
            for row in rows {
                let chunk = row?;
                found.insert(chunk.chunk_ref.clone(), chunk);
            }
        }
        Ok(refs
            .iter()
            .map(|stable_ref| found[stable_ref].clone())
            .collect())
    }
    /// The passages either side of each named one in its own rendition: up to
    /// `before` immediately preceding it and up to `after` immediately
    /// following it, each side in reading order. One entry per requested ref,
    /// in request order.
    ///
    /// A window is cut short only by the start or the end of the document. An
    /// ordinal inside the document that holds no passage means the rendition
    /// is broken, and is refused rather than answered around: a caller reading
    /// "the passage before" has no way to tell that one was skipped.
    ///
    /// Every row holding a ref holds the same rendition — a ref is a hash of
    /// the rendition and the ordinal — so when one is indexed under two file
    /// rows either answers.
    pub fn managed_chunk_neighbors(
        &self,
        refs: &[ChunkRef],
        before: usize,
        after: usize,
    ) -> anyhow::Result<Vec<ManagedChunkNeighbors>> {
        let wanted: HashSet<ChunkRef> = refs.iter().cloned().collect();
        self.resolve_chunk_refs(&wanted)?;

        const REFS_PER_QUERY: usize = 300;
        let unique: Vec<ChunkRef> = wanted.into_iter().collect();
        let mut located: HashMap<ChunkRef, (i64, usize)> = HashMap::with_capacity(unique.len());
        for batch in unique.chunks(REFS_PER_QUERY) {
            let placeholders = vec!["?"; batch.len()].join(",");
            let mut stmt = self.conn.prepare(&format!(
                "SELECT chunk_ref, file_id, chunk_idx
                 FROM chunks WHERE chunk_ref IN ({placeholders})"
            ))?;
            let rows = stmt.query_map(
                params_from_iter(batch.iter().map(ChunkRef::as_str)),
                |row| {
                    Ok((
                        ChunkRef(row.get(0)?),
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)? as usize,
                    ))
                },
            )?;
            for row in rows {
                let (stable_ref, file_id, ordinal) = row?;
                located.entry(stable_ref).or_insert((file_id, ordinal));
            }
        }

        // The last ordinal of each document, so a window that reaches past
        // either end is cut there, and every ordinal any window reaches.
        let mut last_ordinal: HashMap<i64, usize> = HashMap::new();
        for &(file_id, _) in located.values() {
            if let std::collections::hash_map::Entry::Vacant(entry) = last_ordinal.entry(file_id) {
                let last: i64 = self.conn.query_row(
                    "SELECT MAX(chunk_idx) FROM chunks WHERE file_id = ?1",
                    params![file_id],
                    |row| row.get(0),
                )?;
                entry.insert(last as usize);
            }
        }
        let window = |file_id: i64, ordinal: usize| {
            let last = last_ordinal[&file_id];
            (
                ordinal.saturating_sub(before)..ordinal,
                ordinal + 1..ordinal.saturating_add(after).min(last).saturating_add(1),
            )
        };
        let mut reached: BTreeMap<i64, std::collections::BTreeSet<usize>> = BTreeMap::new();
        for &(file_id, ordinal) in located.values() {
            let (preceding, following) = window(file_id, ordinal);
            reached
                .entry(file_id)
                .or_default()
                .extend(preceding.chain(following));
        }

        const ORDINALS_PER_QUERY: usize = 400;
        let mut passages: HashMap<(i64, usize), ManagedChunkData> = HashMap::new();
        for (file_id, ordinals) in &reached {
            let ordinals: Vec<i64> = ordinals.iter().map(|&ordinal| ordinal as i64).collect();
            for batch in ordinals.chunks(ORDINALS_PER_QUERY) {
                let placeholders = vec!["?"; batch.len()].join(",");
                let mut stmt = self.conn.prepare(&format!(
                    "SELECT {MANAGED_CHUNK_COLUMNS}
                     FROM chunks WHERE file_id = ? AND chunk_idx IN ({placeholders})"
                ))?;
                let bound: Vec<&dyn ToSql> = std::iter::once(file_id as &dyn ToSql)
                    .chain(batch.iter().map(|ordinal| ordinal as &dyn ToSql))
                    .collect();
                let rows = stmt.query_map(bound.as_slice(), managed_chunk_from_row)?;
                for row in rows {
                    let chunk = row?;
                    passages.insert((*file_id, chunk.ordinal), chunk);
                }
            }
        }

        let take = |file_id: i64, ordinals: std::ops::Range<usize>, of: &ChunkRef| {
            ordinals
                .map(|ordinal| {
                    passages.get(&(file_id, ordinal)).cloned().ok_or_else(|| {
                        anyhow::anyhow!(
                            "The rendition holding {} has no passage at ordinal {ordinal}, \
                             inside the document; it needs to be re-indexed.",
                            of.as_str()
                        )
                    })
                })
                .collect::<anyhow::Result<Vec<_>>>()
        };
        refs.iter()
            .map(|stable_ref| {
                let (file_id, ordinal) = located[stable_ref];
                let (preceding, following) = window(file_id, ordinal);
                Ok(ManagedChunkNeighbors {
                    of: stable_ref.clone(),
                    before: take(file_id, preceding, stable_ref)?,
                    after: take(file_id, following, stable_ref)?,
                })
            })
            .collect()
    }
    pub fn accumulate_chunk_refs(
        &self,
        groups: &[Vec<ChunkRef>],
    ) -> anyhow::Result<Vec<ChunkAccumulation>> {
        anyhow::ensure!(
            groups.iter().all(|group| !group.is_empty()),
            "An aggregate over no chunks is not a vector"
        );
        let wanted: HashSet<ChunkRef> = groups.iter().flatten().cloned().collect();
        if wanted.is_empty() {
            return Ok(Vec::new());
        }
        let rowids = self.resolve_chunk_refs(&wanted)?;
        let rowid_groups: Vec<Vec<i64>> = groups
            .iter()
            .map(|group| group.iter().map(|stable_ref| rowids[stable_ref]).collect())
            .collect();
        self.accumulate_chunk_ids(&rowid_groups, "Aggregate request")
    }
    /// The stored vectors of named chunks, L2-normalized, keyed by chunk id.
    ///
    /// Shared by every endpoint that answers a question *about* named chunks
    /// rather than handing the chunks out, because the rule they all need is
    /// the same one and it is not a detail: a chunk id this index does not
    /// hold refuses the whole request, naming the ids. Chunk ids are rowids
    /// reissued when a file is re-indexed, so the alternative is answering
    /// over whichever of the caller's ids happened to survive — a perfectly
    /// well-formed number with nothing in it to say which passages it is
    /// about.
    ///
    /// `asked_for` opens the refusal message, so a caller reads "Centroid
    /// request names 3 chunks…" rather than a sentence that could have come
    /// from any of them.
    pub(super) fn normalized_chunk_vectors(
        &self,
        wanted: &HashSet<i64>,
        asked_for: &str,
    ) -> anyhow::Result<HashMap<i64, Vec<f32>>> {
        // Batched rather than one IN-list: the number of ids is the caller's,
        // and SQLite's bound-variable ceiling is the build's. A limit that
        // depends on how the library was compiled is not one this can honour
        // by asserting.
        const IDS_PER_QUERY: usize = 500;
        let ids: Vec<i64> = wanted.iter().copied().collect();
        let mut normalized: HashMap<i64, Vec<f32>> = HashMap::with_capacity(ids.len());
        for batch in ids.chunks(IDS_PER_QUERY) {
            let placeholders = vec!["?"; batch.len()].join(",");
            let mut stmt = self.conn.prepare(&format!(
                "SELECT c.id, v.embedding
                 FROM chunks c
                 JOIN vec_chunks v ON v.rowid = c.id
                 WHERE c.id IN ({placeholders})"
            ))?;
            let rows = stmt.query_map(params_from_iter(batch.iter()), |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?))
            })?;
            for row in rows {
                let (chunk_id, blob) = row?;
                let embedding = f32_slice_from_bytes(&blob)?;
                anyhow::ensure!(
                    embedding.len() == self.dimension,
                    "Stored embedding dimension mismatch for chunk {}. Expected {}, received {}.",
                    chunk_id,
                    self.dimension,
                    embedding.len()
                );
                normalized.insert(chunk_id, normalized_vector(&embedding));
            }
        }

        if normalized.len() != wanted.len() {
            let mut missing: Vec<i64> = wanted
                .iter()
                .filter(|id| !normalized.contains_key(id))
                .copied()
                .collect();
            missing.sort_unstable();
            let shown: Vec<String> = missing.iter().take(10).map(i64::to_string).collect();
            anyhow::bail!(
                "{} names {} chunk{} this index does not hold ({}{}) — they were re-indexed \
                 since those ids were recorded.",
                asked_for,
                missing.len(),
                if missing.len() == 1 { "" } else { "s" },
                shown.join(", "),
                if missing.len() > shown.len() {
                    ", …"
                } else {
                    ""
                },
            );
        }
        Ok(normalized)
    }
    pub(super) fn accumulate_chunk_ids(
        &self,
        groups: &[Vec<i64>],
        asked_for: &str,
    ) -> anyhow::Result<Vec<ChunkAccumulation>> {
        anyhow::ensure!(
            groups.iter().all(|group| !group.is_empty()),
            "An aggregate over no chunks is not a vector"
        );
        let wanted: HashSet<i64> = groups.iter().flatten().copied().collect();
        if wanted.is_empty() {
            return Ok(Vec::new());
        }
        let normalized = self.normalized_chunk_vectors(&wanted, asked_for)?;
        Ok(groups
            .iter()
            .map(|group| {
                let mut sum = vec![0.0; self.dimension];
                for chunk_id in group {
                    for (total, value) in sum.iter_mut().zip(&normalized[chunk_id]) {
                        *total += value;
                    }
                }
                ChunkAccumulation {
                    sum,
                    member_count: group.len(),
                }
            })
            .collect())
    }
    /// How close each probe vector sits to a named set of chunks, both ways
    /// round, without any chunk vector leaving this index.
    ///
    /// Two answers from one scan, because they are two readings of the same
    /// matrix and a caller that asked for them separately would pay for it
    /// twice:
    ///
    /// * per probe, the nearest chunk of `chunk_ids` and how near it is;
    /// * per chunk, the nearest probe and how near it is.
    ///
    /// Plus, per probe, the **mean** similarity over its own `scope` — a
    /// second set of chunk ids, unrelated to `chunk_ids`. That exists because
    /// the consumer this was built for (Underdog's §6 conceptual coverage)
    /// judges a reading against a bar it derives from a different set of
    /// passages, and computing that bar on its side would mean receiving the
    /// vectors this function exists to keep here.
    ///
    /// **The probe vector is used exactly as given.** It is dotted with the
    /// L2-normalized chunk vector and nothing else is done to it, so a
    /// normalized probe yields a cosine and an unnormalized mean of unit
    /// vectors yields the mean of the cosines of its members. Normalizing here
    /// would quietly destroy the second, which is the whole of how a caller
    /// asks "how close is this *group* to these passages" in one probe.
    ///
    /// Chunk ids this index does not hold — in `chunk_ids` or in any scope —
    /// refuse the whole request, for the reason
    /// [`Self::normalized_chunk_vectors`] states.
    pub fn chunk_similarity(
        &self,
        probes: &[SimilarityProbe],
        chunk_ids: &[i64],
    ) -> anyhow::Result<ChunkSimilarity> {
        for (at, probe) in probes.iter().enumerate() {
            anyhow::ensure!(
                probe.vector.len() == self.dimension,
                "Probe {} has dimension {}; this index embeds at {}.",
                at,
                probe.vector.len(),
                self.dimension
            );
        }
        let wanted: HashSet<i64> = chunk_ids
            .iter()
            .copied()
            .chain(probes.iter().flat_map(|probe| probe.scope.iter().copied()))
            .collect();
        if wanted.is_empty() || probes.is_empty() {
            return Ok(ChunkSimilarity::default());
        }
        let normalized = self.normalized_chunk_vectors(&wanted, "Similarity request")?;

        // Nearest-probe per chunk accumulates as the probes are walked, so the
        // matrix is never materialised: the reply is O(probes + chunks), which
        // is what makes asking about a whole document affordable.
        let mut chunks: Vec<ChunkNearest> = chunk_ids
            .iter()
            .map(|chunk_id| ChunkNearest {
                chunk_id: *chunk_id,
                probe: 0,
                similarity: f32::NEG_INFINITY,
            })
            .collect();
        let mut answers = Vec::with_capacity(probes.len());
        for (at, probe) in probes.iter().enumerate() {
            let mut nearest: Option<(i64, f32)> = None;
            for held in chunks.iter_mut() {
                let score = dot(&probe.vector, &normalized[&held.chunk_id]);
                // Strictly greater, so a tie keeps the earlier probe and the
                // earlier chunk — the caller's order is the tiebreak, and two
                // runs of one request cannot disagree about it.
                if score > held.similarity {
                    held.similarity = score;
                    held.probe = at;
                }
                if nearest.is_none_or(|(_, best)| score > best) {
                    nearest = Some((held.chunk_id, score));
                }
            }
            let scope_mean = (!probe.scope.is_empty()).then(|| {
                let total: f32 = probe
                    .scope
                    .iter()
                    .map(|chunk_id| dot(&probe.vector, &normalized[chunk_id]))
                    .sum();
                total / probe.scope.len() as f32
            });
            answers.push(ProbeSimilarity {
                nearest_chunk_id: nearest.map(|(chunk_id, _)| chunk_id),
                similarity: nearest.map(|(_, score)| score),
                scope_mean,
                scope_size: probe.scope.len(),
            });
        }

        Ok(ChunkSimilarity {
            probes: answers,
            chunks,
        })
    }
    /// Nearest managed chunks across the entire corpus for each caller-owned
    /// probe, scored as cosines: the probe is normalized here and the space
    /// is cosine by construction (`distance_metric=cosine`), so similarity is
    /// `1 - distance` and probe magnitude never moves a score.
    ///
    /// Answered by the vector table's own nearest-neighbour scan, once per
    /// probe. It used to read every stored vector out through a join on
    /// `vec_chunks.rowid`, and sqlite-vec serves a column read by reopening
    /// the vector chunk's blob and walking its overflow-page chain from the
    /// start — quadratic in the chunk, and on a 6,032-passage, 1,024-wide
    /// projection about seven seconds of `pread` per search where the scan
    /// itself costs thirty milliseconds.
    ///
    /// Ties keep the documented order (similarity, then rendition, then
    /// ordinal) exactly: when the last neighbour fetched ties the last one
    /// returned, more are fetched until the tie is closed, so which of several
    /// equal passages falls inside `top_k` never depends on the table's scan
    /// order.
    pub fn managed_chunk_search(
        &self,
        probes: &[Vec<f32>],
        top_k: usize,
        min_similarity: f32,
    ) -> anyhow::Result<Vec<Vec<ManagedChunkSearchHit>>> {
        anyhow::ensure!(!probes.is_empty(), "Search request names no probes");
        anyhow::ensure!(top_k > 0, "Search top_k must be greater than zero");
        anyhow::ensure!(
            top_k <= MANAGED_SEARCH_MAX_NEIGHBOURS,
            "Search top_k {top_k} exceeds {MANAGED_SEARCH_MAX_NEIGHBOURS}"
        );
        anyhow::ensure!(
            min_similarity.is_finite(),
            "Search minimum similarity is not finite"
        );
        for (at, probe) in probes.iter().enumerate() {
            anyhow::ensure!(
                probe.len() == self.dimension,
                "Probe {} has dimension {}; this index embeds at {}.",
                at,
                probe.len(),
                self.dimension
            );
            anyhow::ensure!(
                probe.iter().all(|value| value.is_finite()),
                "Probe {at} contains a non-finite value"
            );
            anyhow::ensure!(
                probe.iter().any(|value| *value != 0.0),
                "Probe {at} has zero magnitude and names no direction"
            );
        }

        let mut neighbours = self.conn.prepare(
            "SELECT v.rowid, v.distance
               FROM vec_chunks v
              WHERE v.embedding MATCH ?1
                AND v.k = ?2
              ORDER BY v.distance",
        )?;
        let mut describe = self.conn.prepare(
            "SELECT c.chunk_ref, f.snapshot_id, f.rendition_id, c.chunk_idx
               FROM chunks c
               JOIN files f ON f.id = c.file_id
              WHERE c.id = ?1",
        )?;
        let mut answers = Vec::with_capacity(probes.len());
        for probe in probes {
            let blob = f32_slice_to_bytes(&normalized_vector(probe));
            // One past the edge, so the first fetch can already show whether
            // the neighbour after the last one returned ties it.
            let mut fetch = (top_k + 1).min(MANAGED_SEARCH_MAX_NEIGHBOURS);
            let scored = loop {
                let scored = neighbours
                    .query_map(params![blob, fetch as i64], |row| {
                        Ok((row.get::<_, i64>(0)?, row.get::<_, f64>(1)?))
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                // Fewer back than asked for: the table holds nothing further.
                let exhausted = scored.len() < fetch;
                // The last neighbour fetched is strictly farther than the
                // last one that can be returned, so nothing past it ties.
                let closed =
                    scored.len() > top_k && scored[scored.len() - 1].1 > scored[top_k - 1].1;
                if exhausted || closed {
                    break scored;
                }
                if fetch >= MANAGED_SEARCH_MAX_NEIGHBOURS {
                    tracing::warn!(
                        "managed_chunk_search: {MANAGED_SEARCH_MAX_NEIGHBOURS} or more passages \
                         tie at the top_k boundary; which of them are returned is the vector \
                         table's scan order"
                    );
                    break scored;
                }
                fetch = (fetch * 2).min(MANAGED_SEARCH_MAX_NEIGHBOURS);
            };
            let mut hits = Vec::with_capacity(scored.len());
            for (rowid, distance) in scored {
                consumer_ensure!(
                    distance.is_finite(),
                    ConsumerErrorCode::DocumentIndexIncomplete,
                    "invalid stored vector",
                );
                let similarity = (1.0 - distance) as f32;
                if similarity < min_similarity {
                    continue;
                }
                let described = describe
                    .query_row(params![rowid], |row| {
                        Ok((
                            row.get::<_, Option<String>>(0)?,
                            row.get::<_, Option<String>>(1)?,
                            row.get::<_, Option<String>>(2)?,
                            row.get::<_, i64>(3)?,
                        ))
                    })
                    .optional()?;
                let Some((chunk_ref, snapshot_id, rendition_id, ordinal)) = described else {
                    consumer_bail!(
                        ConsumerErrorCode::DocumentIndexIncomplete,
                        "stored vector {rowid} belongs to no chunk",
                    );
                };
                let chunk_ref = chunk_ref.ok_or_else(|| {
                    consumer_error(
                        ConsumerErrorCode::DocumentIndexIncomplete,
                        "chunk ref is absent",
                    )
                })?;
                let snapshot_id = snapshot_id.ok_or_else(|| {
                    consumer_error(
                        ConsumerErrorCode::DocumentIndexIncomplete,
                        "snapshot id is absent",
                    )
                })?;
                let rendition_id = rendition_id.ok_or_else(|| {
                    consumer_error(
                        ConsumerErrorCode::DocumentIndexIncomplete,
                        "rendition id is absent",
                    )
                })?;
                consumer_ensure!(
                    ordinal >= 0,
                    ConsumerErrorCode::DocumentIndexIncomplete,
                    "negative chunk ordinal",
                );
                hits.push(ManagedChunkSearchHit {
                    chunk_ref: ChunkRef(chunk_ref),
                    snapshot_id: DocumentSnapshotId(snapshot_id),
                    rendition_id: RenditionId(rendition_id),
                    ordinal: ordinal as usize,
                    similarity,
                });
            }
            hits.sort_by(|left, right| {
                right
                    .similarity
                    .total_cmp(&left.similarity)
                    .then(left.rendition_id.as_str().cmp(right.rendition_id.as_str()))
                    .then(left.ordinal.cmp(&right.ordinal))
            });
            hits.truncate(top_k);
            answers.push(hits);
        }
        Ok(answers)
    }
}

/// One question for [`SemanticIndex::chunk_similarity`]: a vector in the
/// index's own space, and optionally a set of chunk ids to average it over.
///
/// `scope` is not a filter on the search — the nearest-chunk answer always
/// ranges over the request's whole `chunk_ids`. It is a separate question
/// asked in the same pass, because the caller that needs it needs both about
/// the same probe.
#[derive(Clone, Debug)]
pub struct SimilarityProbe {
    pub vector: Vec<f32>,
    pub scope: Vec<i64>,
}

/// What one probe found. `None` for the nearest pair when the request named no
/// chunks to search, which is a legitimate request: a caller may want only the
/// scope means.
#[derive(Clone, Debug)]
pub struct ProbeSimilarity {
    pub nearest_chunk_id: Option<i64>,
    pub similarity: Option<f32>,
    pub scope_mean: Option<f32>,
    /// How many ids the mean was taken over, reported so a caller can tell a
    /// mean of two from a mean of two hundred without keeping its own copy of
    /// what it asked for.
    pub scope_size: usize,
}

/// What one chunk found: the probe it sits closest to, by index into the
/// request's probe list.
#[derive(Clone, Debug)]
pub struct ChunkNearest {
    pub chunk_id: i64,
    pub probe: usize,
    pub similarity: f32,
}

/// One full-corpus vector hit addressed entirely by managed, stable ids.
#[derive(Clone, Debug, PartialEq)]
pub struct ManagedChunkSearchHit {
    pub chunk_ref: ChunkRef,
    pub snapshot_id: DocumentSnapshotId,
    pub rendition_id: RenditionId,
    pub ordinal: usize,
    pub similarity: f32,
}

/// Both directions of one similarity request. `chunks` follows the order the
/// ids were asked in; `probes` follows the probe order.
#[derive(Clone, Debug, Default)]
pub struct ChunkSimilarity {
    pub probes: Vec<ProbeSimilarity>,
    pub chunks: Vec<ChunkNearest>,
}
