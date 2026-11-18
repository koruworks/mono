// SPDX-License-Identifier: MPL-2.0
use crate::{Error, carrier};
use std::sync::mpsc;

pub(super) struct Request {
    pub command: carrier::Command,
    pub reply: mpsc::SyncSender<Result<carrier::Reply, Error>>,
}
#[derive(Clone)]
pub(super) struct Client(pub mpsc::SyncSender<Request>);
impl Client {
    pub fn call(&self, command: carrier::Command) -> Result<carrier::Reply, Error> {
        let (reply, answer) = mpsc::sync_channel(1);
        self.0
            .try_send(Request { command, reply })
            .map_err(|_| Error::EngineUnavailable)?;
        match answer
            .recv_timeout(super::super::TIMEOUT)
            .map_err(|_| Error::EngineUnavailable)??
        {
            carrier::Reply::Refused(error) => Err(error),
            reply => Ok(reply),
        }
    }
    pub fn done(&self, command: carrier::Command) -> Result<(), Error> {
        match self.call(command)? {
            carrier::Reply::Done => Ok(()),
            _ => Err(Error::InvalidRequest),
        }
    }
}
