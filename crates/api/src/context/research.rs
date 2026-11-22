//! Tags, smart collections and the search log.

use super::*;

impl AppContext {
    pub(super) fn research_store(&self) -> anyhow::Result<Arc<Mutex<ResearchStore>>> {
        let mut guard = self.research_store.lock();
        if let Some(store) = guard.as_ref() {
            return Ok(Arc::clone(store));
        }
        let store = Arc::new(Mutex::new(ResearchStore::open(
            &self.data_dir,
            &self.bookmarks_path,
        )?));
        *guard = Some(Arc::clone(&store));
        Ok(store)
    }
    pub fn list_tags(&self) -> anyhow::Result<Vec<Tag>> {
        self.research_store()?
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .list_tags()
    }
    pub fn create_tag(&self, new: NewTag) -> anyhow::Result<Tag> {
        let tag = self
            .research_store()?
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .create_tag(new)?;
        self.events
            .emit("research-state-updated", serde_json::json!({"kind":"tags"}));
        Ok(tag)
    }
    pub fn update_tag(&self, id: &str, update: UpdateTag) -> anyhow::Result<Tag> {
        let tag = self
            .research_store()?
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .update_tag(id, update)?;
        self.events
            .emit("research-state-updated", serde_json::json!({"kind":"tags"}));
        Ok(tag)
    }
    pub fn delete_tag(&self, id: &str) -> anyhow::Result<()> {
        self.research_store()?
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .delete_tag(id)?;
        self.events
            .emit("research-state-updated", serde_json::json!({"kind":"tags"}));
        Ok(())
    }
    pub fn update_document_tags(&self, update: DocumentTagUpdate) -> anyhow::Result<()> {
        self.research_store()?
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .update_document_tags(update)?;
        self.events.emit(
            "research-state-updated",
            serde_json::json!({"kind":"document_tags"}),
        );
        Ok(())
    }
    pub fn validate_collection(&self, expression: &str) -> CollectionValidation {
        ResearchStore::validate_collection(expression)
    }
    pub fn list_collections(&self) -> anyhow::Result<Vec<SmartCollection>> {
        self.research_store()?
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .list_collections()
    }
    pub fn create_collection(&self, new: NewSmartCollection) -> anyhow::Result<SmartCollection> {
        let item = self
            .research_store()?
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .create_collection(new)?;
        self.events.emit(
            "research-state-updated",
            serde_json::json!({"kind":"collections"}),
        );
        Ok(item)
    }
    pub fn update_collection(
        &self,
        id: &str,
        update: UpdateSmartCollection,
    ) -> anyhow::Result<SmartCollection> {
        let item = self
            .research_store()?
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .update_collection(id, update)?;
        self.events.emit(
            "research-state-updated",
            serde_json::json!({"kind":"collections"}),
        );
        Ok(item)
    }
    pub fn delete_collection(&self, id: &str) -> anyhow::Result<()> {
        self.research_store()?
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .delete_collection(id)?;
        self.events.emit(
            "research-state-updated",
            serde_json::json!({"kind":"collections"}),
        );
        Ok(())
    }
    pub fn list_search_log(&self, limit: usize) -> anyhow::Result<Vec<SearchLogEntry>> {
        self.research_store()?
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .list_search_log(limit)
    }
    pub fn delete_search_log(&self, id: &str) -> anyhow::Result<()> {
        self.research_store()?
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .delete_search_log(id)
    }
    pub fn clear_search_log(&self) -> anyhow::Result<()> {
        self.research_store()?
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clear_search_log()
    }
}
