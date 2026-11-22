use super::*;

/// The forallx failure: a text appears in two chunks and its vector was
/// adopted. Both chunks must get it, and nothing is queued for embedding.
#[test]
fn a_repeated_adopted_text_fills_every_chunk_that_carries_it() {
    let texts = ["boilerplate", "unique", "boilerplate"];
    let hashes = hashes_of(&texts);
    let mut vectors = HashMap::new();
    vectors.insert(hashes[0].clone(), vec![1.0, 0.0]);
    vectors.insert(hashes[1].clone(), vec![0.0, 1.0]);

    assert!(texts_to_embed(texts.iter().copied(), &hashes, &vectors).is_empty());

    let mut chunks: Vec<((), Vec<f32>)> = texts.iter().map(|_| ((), Vec::new())).collect();
    fill_chunk_vectors(&mut chunks, &hashes, &vectors).unwrap();
    let filled: Vec<Vec<f32>> = chunks.into_iter().map(|(_, v)| v).collect();
    assert_eq!(filled, vec![vec![1.0, 0.0], vec![0.0, 1.0], vec![1.0, 0.0]]);
}

/// A text repeated among the chunks nothing was adopted for is embedded
/// once, and the one result fills each chunk carrying it.
#[test]
fn a_repeated_missing_text_is_embedded_once_and_fills_every_copy() {
    let texts = ["adopted", "repeated", "fresh", "repeated"];
    let hashes = hashes_of(&texts);
    let mut vectors = HashMap::new();
    vectors.insert(hashes[0].clone(), vec![9.0]);

    let queued = texts_to_embed(texts.iter().copied(), &hashes, &vectors);
    assert_eq!(
        queued,
        vec![
            (hashes[1].clone(), "repeated".to_string()),
            (hashes[2].clone(), "fresh".to_string()),
        ]
    );

    vectors.extend(
        queued
            .into_iter()
            .map(|(hash, _)| hash)
            .zip([vec![1.0], vec![2.0]]),
    );
    let mut chunks: Vec<((), Vec<f32>)> = texts.iter().map(|_| ((), Vec::new())).collect();
    fill_chunk_vectors(&mut chunks, &hashes, &vectors).unwrap();
    let filled: Vec<Vec<f32>> = chunks.into_iter().map(|(_, v)| v).collect();
    assert_eq!(filled, vec![vec![9.0], vec![1.0], vec![2.0], vec![1.0]]);
}

#[test]
fn a_chunk_with_no_vector_for_its_text_is_refused() {
    let hashes = hashes_of(&["held", "absent"]);
    let mut vectors = HashMap::new();
    vectors.insert(hashes[0].clone(), vec![1.0]);
    let mut chunks: Vec<((), Vec<f32>)> = vec![((), Vec::new()), ((), Vec::new())];
    let error = fill_chunk_vectors(&mut chunks, &hashes, &vectors).unwrap_err();
    assert_eq!(
        error.code(),
        Some(ConsumerErrorCode::DocumentIndexIncomplete)
    );
}

#[tokio::test]
async fn embed_texts_returns_pinned_identity_and_validated_vectors() {
    let (_dir, ctx) = test_ctx();

    // No embedder loaded: loud, actionable error.
    let err = ctx
        .embed_texts(vec!["a".to_string()], None)
        .await
        .unwrap_err();
    assert!(err.contains("Semantic model unavailable"), "{err}");

    *ctx.embedder.lock() = Some(Arc::new(TopicEmbedder {
        calls: Arc::new(AtomicUsize::new(0)),
    }));

    let err = ctx.embed_texts(Vec::new(), None).await.unwrap_err();
    assert!(err.contains("No texts provided"), "{err}");

    let result = ctx
        .embed_texts(
            vec!["cat concept".to_string(), "dog concept".to_string()],
            Some("space-under-test".to_string()),
        )
        .await
        .unwrap();
    assert_eq!(
        result.embedding_space_id.as_deref(),
        Some("space-under-test"),
        "the reply names the space the caller was verified against"
    );
    assert_eq!(result.model_id, "topic-test");
    assert_eq!(result.dimension, 2);
    assert_eq!(result.vectors.len(), 2);
    assert_eq!(result.vectors[0], vec![1.0, 0.0]);
    assert_eq!(result.vectors[1], vec![0.0, 1.0]);
}

#[tokio::test]
async fn embed_query_texts_answers_in_the_query_role() {
    let (_dir, ctx) = test_ctx();

    let err = ctx
        .embed_query_texts(vec!["a".to_string()], None)
        .await
        .unwrap_err();
    assert!(err.contains("Semantic model unavailable"), "{err}");

    *ctx.embedder.lock() = Some(Arc::new(RoleEmbedder));

    let err = ctx.embed_query_texts(Vec::new(), None).await.unwrap_err();
    assert!(err.contains("No texts provided"), "{err}");
    let err = ctx
        .embed_query_texts(vec!["what".to_string(), "  ".to_string()], None)
        .await
        .unwrap_err();
    assert!(err.contains("empty"), "{err}");

    let texts = vec!["what binds".to_string(), "why it holds".to_string()];
    let query = ctx
        .embed_query_texts(texts.clone(), Some("space-under-test".to_string()))
        .await
        .unwrap();
    assert_eq!(
        query.embedding_space_id.as_deref(),
        Some("space-under-test")
    );
    assert_eq!(query.model_id, "role-test");
    assert_eq!(query.dimension, 2);
    assert_eq!(query.vectors, vec![vec![1.0, 0.0], vec![1.0, 0.0]]);

    let passage = ctx.embed_texts(texts, None).await.unwrap();
    assert_ne!(
        passage.vectors, query.vectors,
        "the same words in the passage role are a different vector"
    );
}
