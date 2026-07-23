use super::EngineRunner;

impl EngineRunner {
    pub(super) fn heartbeat_owned_lease(&self) {
        if !self.context.daemon_managed_claim() {
            return;
        }
        let Some(metadata) = self.load_metadata() else {
            return;
        };
        let Some(repository) = metadata.repository.as_deref() else {
            return;
        };
        let Some(issue_number) = metadata.issue_lease_number() else {
            return;
        };
        let conn = self.conn.borrow();
        if let Ok(Some(lease)) =
            crate::persistence::get_lease_for_issue(&conn, repository, issue_number)
        {
            if lease.run_id.as_deref() == Some(self.instance.run_id.as_str()) {
                let _ = crate::persistence::touch_owned_running_lease_heartbeat(
                    &conn,
                    &lease.lease_id,
                    &self.instance.run_id,
                );
            }
        }
    }
}
