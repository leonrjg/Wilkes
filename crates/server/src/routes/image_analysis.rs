use crate::*;

/// Which recognizer a request is about. The engine and model travel together
/// everywhere, because a model id means nothing without the engine that
/// resolves it.
#[derive(Deserialize)]
pub(crate) struct RecognizerQuery {
    pub(crate) engine: wilkes_core::extract::image::dispatch::RecognitionEngine,
    pub(crate) model_id: String,
}

/// Every recognizer this build can read with, and the engines it compiled in.
pub(crate) async fn image_recognizer_catalogue_handler(
    State(state): State<Arc<AppState>>,
) -> Json<wilkes_core::extract::image::dispatch::RecognizerCatalogue> {
    Json(state.context().image_recognizer_catalogue())
}

/// What the named recognizer is, where it came from, and under what licence.
/// Static, and answers whether or not it is installed: it is what the download
/// is disclosed by, so it has to be readable before the download.
pub(crate) async fn image_recognizer_inventory_handler(
    State(state): State<Arc<AppState>>,
    Query(query): Query<RecognizerQuery>,
) -> Result<Json<wilkes_core::types::RecognizerInventory>, (StatusCode, Json<ErrorBody>)> {
    state
        .context()
        .image_recognizer_inventory(query.engine, &query.model_id)
        .map(Json)
        .map_err(|e| server_err(format!("{e:#}")))
}

pub(crate) async fn install_image_recognizer_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<RecognizerQuery>,
) -> Result<Json<bool>, (StatusCode, Json<ErrorBody>)> {
    state
        .context()
        .install_image_recognizer(body.engine, body.model_id)
        .await
        .map(|()| Json(true))
        .map_err(|e| server_err(format!("{e:#}")))
}

/// Download the layout detector. No body: there is one detector and it is not
/// chosen from a catalogue.
#[cfg(feature = "recognize-onnx")]
pub(crate) async fn install_layout_detector_handler(
    State(state): State<Arc<AppState>>,
) -> Result<Json<bool>, (StatusCode, Json<ErrorBody>)> {
    state
        .context()
        .install_layout_detector()
        .await
        .map(|()| Json(true))
        .map_err(|e| server_err(format!("{e:#}")))
}

/// The same route in a build with no detector compiled in, so a client gets an
/// explanation rather than a 404 it would read as a wrong URL.
#[cfg(not(feature = "recognize-onnx"))]
pub(crate) async fn install_layout_detector_handler(
    State(_state): State<Arc<AppState>>,
) -> Result<Json<bool>, (StatusCode, Json<ErrorBody>)> {
    Err(server_err(
        "this build has no layout detector compiled in".to_string(),
    ))
}
