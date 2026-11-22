use crate::*;

/// What this Wilkes can embed with — the models, their dimensions, and the
/// input recipes they require.
///
/// Unscoped, like the catalogue routes and for the same reason: it reads
/// nothing of the user's documents and there is no corpus for it to pin. It
/// describes the host's embedders, and a consumer asks it *before* it has a
/// corpus in the space it is asking about.
pub(crate) async fn embedder_capabilities_handler(
    State(state): State<Arc<AppState>>,
) -> Json<wilkes_core::types::EmbedderCapabilityManifest> {
    let context = state.context();
    let settings = context.get_settings().await;
    Json(context.local_embedder_capabilities(&settings))
}

#[derive(Deserialize)]
pub(crate) struct EmbedTextBody {
    pub(crate) texts: Vec<String>,
    /// Which index's embedder to use, and which space the caller believes it
    /// is talking to.
    ///
    /// A consumer keeping one vector space cannot let "whichever workspace the
    /// user last opened" decide what embeds its text: the model would change
    /// under it when the user switches windows. Pinning the space is how it
    /// says so.
    #[serde(default)]
    pub(crate) scope: ConsumerScope,
    /// A model to embed with, named instead of an index: what another Wilkes
    /// sends when it embeds here. Exclusive with `scope` — a request is about
    /// an index or about a model, and one that named both would leave the
    /// handler to pick which to believe.
    #[serde(default)]
    pub(crate) embedder: Option<EmbedderAddress>,
}

impl EmbedTextBody {
    /// The model this request names, refused when it also names an index.
    pub(crate) fn model_address(
        &self,
    ) -> Result<Option<EmbedderAddress>, (StatusCode, Json<ErrorBody>)> {
        match &self.embedder {
            Some(_) if self.scope != ConsumerScope::default() => Err(err(
                "a request names either an index (scope) or a model (embedder), not both",
            )),
            address => Ok(address.clone()),
        }
    }
}

/// Embed arbitrary strings with the model the addressed index uses, in the
/// **passage** role — the role the vectors it returns are stored under.
///
/// The reply names the embedding space when the index has one, which is what a
/// consumer pins against. It is null only when the addressed workspace holds
/// no index at all: embedding text does not require one.
pub(crate) async fn embed_text_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<EmbedTextBody>,
) -> Result<Json<wilkes_api::context::EmbeddedTexts>, (StatusCode, Json<ErrorBody>)> {
    if body.texts.is_empty() {
        return Err(err("texts must not be empty"));
    }
    if let Some(address) = body.model_address()? {
        return state
            .context()
            .embed_model_texts(address, body.texts)
            .await
            .map(Json)
            .map_err(consumer_err);
    }
    let index = state.consumer_index(&body.scope).await?;
    let space = index.embedding_space_id().map(str::to_string);
    index
        .into_context()
        .embed_texts(body.texts, space)
        .await
        .map(Json)
        .map_err(server_err)
}

/// What `/api/embed/query` answers: the passage route's reply, plus the role
/// the vectors are in, so a caller can refuse a vector it did not ask for.
#[derive(Serialize)]
pub(crate) struct EmbeddedQueries {
    #[serde(flatten)]
    pub(crate) embedded: wilkes_api::context::EmbeddedTexts,
    pub(crate) role: &'static str,
}

/// Embed strings in the **query** role, with the model the addressed index
/// uses. The vectors are for ranking against passage vectors the caller
/// holds and are never for storage: an asymmetric model places a query away
/// from the passage of the same words on purpose.
///
/// A route of its own rather than a role field on `/api/embed/text`: the role
/// is decided by the endpoint a caller reaches, never by a field it sets.
/// Addressed exactly as `/api/embed/text` is, and refused the same way — a
/// managed corpus routes on the pinned space, which must be one it holds and
/// level with it; a model that cannot be loaded is an error, never a vector
/// from some other space.
pub(crate) async fn embed_query_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<EmbedTextBody>,
) -> Result<Json<EmbeddedQueries>, (StatusCode, Json<ErrorBody>)> {
    if body.texts.is_empty() {
        return Err(err("texts must not be empty"));
    }
    if let Some(address) = body.model_address()? {
        return state
            .context()
            .embed_model_query_texts(address, body.texts)
            .await
            .map(|embedded| {
                Json(EmbeddedQueries {
                    embedded,
                    role: "query",
                })
            })
            .map_err(consumer_err);
    }
    let index = state.consumer_index(&body.scope).await?;
    let space = index.embedding_space_id().map(str::to_string);
    index
        .into_context()
        .embed_query_texts(body.texts, space)
        .await
        .map(|embedded| {
            Json(EmbeddedQueries {
                embedded,
                role: "query",
            })
        })
        .map_err(server_err)
}
