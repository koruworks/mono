// SPDX-License-Identifier: MPL-2.0

//! Host tests exercise the actual inbox and wake boundary without a worker,
//! filesystem, gateway, or credentials.

use super::*;

pub struct WorkspaceFixture {
    orders: Receiver<Command>,
    answers: Sender<Answer>,
    flags: Arc<CatalogFlags>,
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl WorkspaceLinks {
    pub fn fixture(wake: Arc<dyn Fn() + Send + Sync>) -> (Self, WorkspaceFixture) {
        let (commands, orders) = channel();
        let (answers_out, answers) = channel();
        let flags = Arc::new(CatalogFlags::default());
        let fixture = WorkspaceFixture {
            orders,
            answers: answers_out,
            flags: flags.clone(),
            wake,
        };
        let links = Self {
            live: Some(Live {
                commands,
                answers,
                flags,
                ended: std::cell::Cell::new(false),
                awaiting: RefCell::default(),
                _inert_orders: None,
                _inert_answers: None,
            }),
        };
        (links, fixture)
    }
}

impl WorkspaceFixture {
    pub fn settle_availability(
        &self,
        result: koru_store_core::errors::Result<koru_api_client::EnsuredWorkspace>,
    ) -> (ProfileId, String, u64) {
        let (id, owner, name, attempt) = self
            .orders
            .try_iter()
            .find_map(|command| match command {
                Command::Keyed(id, command) => match *command {
                    Command::EnsureLive {
                        owner,
                        name,
                        attempt,
                    } => Some((id, owner, name, attempt)),
                    _ => None,
                },
                _ => None,
            })
            .expect("the window sent an availability command");
        assert!(publish_answer(
            &self.answers,
            &*self.wake,
            Answer::Settled(id, Box::new(Answer::WorkspaceEnsured { attempt, result }))
        ));
        (owner, name, attempt)
    }

    pub fn settle_profile(
        &self,
        result: koru_store_core::errors::Result<koru_api_client::CreatedProfile>,
    ) -> koru_api_client::CreateProfileWithWorkspace {
        let (id, request) = self
            .orders
            .try_iter()
            .find_map(|command| match command {
                Command::Keyed(id, command) => match *command {
                    Command::CreateProfile(request) => Some((id, request)),
                    _ => None,
                },
                _ => None,
            })
            .expect("the window sent a Profile creation command");
        assert!(publish_answer(
            &self.answers,
            &*self.wake,
            Answer::Settled(id, Box::new(Answer::ProfileCreated(result)))
        ));
        request
    }

    pub fn catalog(&self, rows: Vec<Row>) {
        assert!(publish_answer(
            &self.answers,
            &*self.wake,
            Answer::Catalog(rows)
        ));
    }

    pub fn invalidate(&self) {
        invalidate_and_wake(&self.flags, &*self.wake, CatalogChange::Workspaces);
    }

    /// Supply the receipt for the next actual keyed request from the host.
    pub fn settle_catalog(&self, rows: Vec<Row>) {
        let id = self
            .orders
            .try_iter()
            .find_map(|command| match command {
                Command::Keyed(id, _) => Some(id),
                _ => None,
            })
            .expect("the window sent a keyed command");
        assert!(publish_answer(
            &self.answers,
            &*self.wake,
            Answer::Settled(id, Box::new(Answer::Catalog(rows)))
        ));
    }

    /// Count catalog fetches since the previous call. For read-only fixtures.
    pub fn take_catalog_requests(&self) -> usize {
        self.orders
            .try_iter()
            .filter(|command| matches!(command, Command::Catalog))
            .count()
    }
}
