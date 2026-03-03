use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct IdentityStoreData {
    pub identities: HashMap<String, Vec<String>>,
}

pub struct IdentityStore {
    data: IdentityStoreData,
    file_path: String,
}

impl IdentityStore {
    pub fn new<P: AsRef<Path>>(path: P) -> Self {
        let file_path = path.as_ref().to_string_lossy().into_owned();
        let data = if let Ok(contents) = fs::read_to_string(&file_path) {
            serde_json::from_str(&contents).unwrap_or_default()
        } else {
            IdentityStoreData::default()
        };

        Self { data, file_path }
    }

    fn save(&self) -> Result<()> {
        let contents = serde_json::to_string_pretty(&self.data)?;
        fs::write(&self.file_path, contents)?;
        Ok(())
    }

    pub fn record_name(&mut self, uid: &str, name: &str) -> Result<bool> {
        let entry = self.data.identities.entry(uid.to_string()).or_default();

        let should_add = match entry.last() {
            Some(last_name) => last_name != name,
            None => true,
        };

        if should_add {
            entry.push(name.to_string());
            self.save()?;
            return Ok(true);
        }

        Ok(false)
    }

    pub fn get_history(&self, uid: &str) -> Option<Vec<String>> {
        self.data.identities.get(uid).cloned()
    }
}
