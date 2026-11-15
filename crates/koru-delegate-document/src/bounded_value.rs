// SPDX-License-Identifier: MPL-2.0

//! Count atomic Loro values before a codec expands them into owned JSON.
//! This bounds the observed value, not a Store open or the document's heap.

use loro::LoroValue;
use serde::Serialize;
use std::io::Write;

pub(crate) const VALUE_DEPTH: usize = 64;
pub(crate) struct Budget(pub(crate) usize);
impl Budget {
    pub(crate) fn take(&mut self, n: usize) -> Result<(), ()> {
        self.0 = self.0.checked_sub(n).ok_or(())?;
        Ok(())
    }
    pub(crate) fn scalar(&mut self, value: &impl Serialize) -> Result<(), ()> {
        serde_json::to_writer(self, value).map_err(|_| ())
    }
    pub(crate) fn value(&mut self, value: &LoroValue, depth: usize) -> Result<(), ()> {
        if depth > VALUE_DEPTH {
            return Err(());
        }
        match value {
            LoroValue::Null => self.take(4),
            LoroValue::Bool(v) => self.scalar(v),
            LoroValue::I64(v) => self.scalar(v),
            LoroValue::Double(v) if v.is_finite() => self.scalar(v),
            LoroValue::String(v) => self.scalar(&v.as_str()),
            LoroValue::List(values) => {
                self.take(2 + values.len().saturating_sub(1))?;
                for value in values.iter() {
                    self.value(value, depth + 1)?;
                }
                Ok(())
            }
            LoroValue::Map(values) => {
                self.take(2 + values.len().saturating_sub(1))?;
                for (key, value) in values.iter() {
                    self.scalar(key)?;
                    self.take(1)?;
                    self.value(value, depth + 1)?;
                }
                Ok(())
            }
            _ => Err(()),
        }
    }
}
impl Write for Budget {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 = self
            .0
            .checked_sub(bytes.len())
            .ok_or_else(|| std::io::Error::other("document value limit"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
