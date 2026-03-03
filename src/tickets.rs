use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum TicketStatus {
    Open,
    Answered,
    Closed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ticket {
    pub id: u64,
    pub creator_uid: String,
    pub creator_name: String,
    pub content: String,
    pub status: TicketStatus,
    pub created_at: String, // ISO 8601 string or simple timestamp
    pub response: Option<String>,
    #[serde(default)]
    pub claimed_by: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TicketStoreData {
    pub next_id: u64,
    pub tickets: Vec<Ticket>,
}

impl Default for TicketStoreData {
    fn default() -> Self {
        Self {
            next_id: 1,
            tickets: Vec::new(),
        }
    }
}

pub struct TicketStore {
    data: TicketStoreData,
    file_path: String,
}

impl TicketStore {
    pub fn new<P: AsRef<Path>>(path: P) -> Self {
        let file_path = path.as_ref().to_string_lossy().into_owned();
        let data = if let Ok(contents) = fs::read_to_string(&file_path) {
            serde_json::from_str(&contents).unwrap_or_default()
        } else {
            TicketStoreData::default()
        };

        Self { data, file_path }
    }

    fn save(&self) -> Result<()> {
        let contents = serde_json::to_string_pretty(&self.data)?;
        fs::write(&self.file_path, contents)?;
        Ok(())
    }

    pub fn create_ticket(&mut self, uid: String, name: String, content: String) -> Result<u64> {
        let id = self.data.next_id;
        self.data.next_id += 1;

        let ticket = Ticket {
            id,
            creator_uid: uid,
            creator_name: name,
            content,
            status: TicketStatus::Open,
            created_at: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            response: None,
            claimed_by: None,
        };

        self.data.tickets.push(ticket);
        self.save()?;
        Ok(id)
    }

    pub fn get_open_tickets(&self) -> Vec<Ticket> {
        self.data
            .tickets
            .iter()
            .filter(|t| t.status == TicketStatus::Open || t.status == TicketStatus::Answered)
            .cloned()
            .collect()
    }

    pub fn get_ticket(&self, id: u64) -> Option<Ticket> {
        self.data.tickets.iter().find(|t| t.id == id).cloned()
    }

    pub fn reply_ticket(
        &mut self,
        id: u64,
        response: String,
        from_admin: bool,
    ) -> Result<Option<Ticket>> {
        let ticket_opt = self.data.tickets.iter_mut().find(|t| t.id == id);
        if let Some(ticket) = ticket_opt {
            let previous = ticket.response.clone().unwrap_or_default();
            let prefix = if from_admin {
                "\n\n**Admin:** "
            } else {
                "\n\n**User:** "
            };
            let new_response = if previous.is_empty() {
                response.clone()
            } else {
                format!("{}{}{}", previous, prefix, response)
            };

            ticket.response = Some(new_response);
            ticket.status = if from_admin {
                TicketStatus::Answered
            } else {
                TicketStatus::Open
            };
            let cloned_ticket = ticket.clone();
            self.save()?;
            return Ok(Some(cloned_ticket));
        }
        Ok(None)
    }

    pub fn claim_ticket(&mut self, id: u64, admin_name: String) -> Result<Option<Ticket>> {
        if let Some(ticket) = self.data.tickets.iter_mut().find(|t| t.id == id) {
            ticket.claimed_by = Some(admin_name);
            let cloned = ticket.clone();
            self.save()?;
            return Ok(Some(cloned));
        }
        Ok(None)
    }

    pub fn get_unread_tickets(&self, uid: &str) -> Vec<Ticket> {
        self.data
            .tickets
            .iter()
            .filter(|t| t.creator_uid == uid && t.status == TicketStatus::Answered)
            .cloned()
            .collect()
    }

    pub fn get_user_history(&self, name_or_uid: &str) -> Vec<Ticket> {
        self.data
            .tickets
            .iter()
            .filter(|t| {
                t.status == TicketStatus::Closed
                    && (t.creator_name.to_lowercase() == name_or_uid.to_lowercase()
                        || t.creator_uid == name_or_uid)
            })
            .cloned()
            .collect()
    }

    pub fn close_ticket(&mut self, id: u64) -> Result<bool> {
        if let Some(ticket) = self.data.tickets.iter_mut().find(|t| t.id == id) {
            ticket.status = TicketStatus::Closed;
            self.save()?;
            return Ok(true);
        }
        Ok(false)
    }
}
