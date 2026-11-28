// SPDX-License-Identifier: MPL-2.0

//! Experimental remapping of alpha coverage before white compositing.
#[derive(Clone, Copy, Debug)]
pub struct Coverage {
    pub gamma: f32,
    pub cutoff: f32,
}

impl Default for Coverage {
    fn default() -> Self {
        Self {
            gamma: 1.0,
            cutoff: 0.0,
        }
    }
}

impl Coverage {
    pub fn active(self) -> bool {
        self.gamma != 1.0 || self.cutoff != 0.0
    }

    pub fn from_args(args: &[String]) -> anyhow::Result<Self> {
        let mut settings = Self::default();
        for arg in args {
            if let Some(value) = arg.strip_prefix("--coverage-gamma=") {
                settings.gamma = value.parse()?;
                anyhow::ensure!(
                    settings.gamma.is_finite() && (0.0..=3.0).contains(&settings.gamma),
                    "coverage gamma must be 0.0..=3.0"
                );
            } else if let Some(value) = arg.strip_prefix("--coverage-cutoff=") {
                settings.cutoff = value.parse()?;
                anyhow::ensure!(
                    settings.cutoff.is_finite() && (0.0..=0.5).contains(&settings.cutoff),
                    "coverage cutoff must be 0.0..=0.5"
                );
            } else {
                anyhow::bail!(
                    "Unknown argument {arg}. Usage: koru.bat font-test [--font=NAME] [--color=#RRGGBB] [--fonts-dir=PATH] [--list-fonts] [--font-report] [--coverage-gamma=1.0] [--coverage-cutoff=0.0]"
                );
            }
        }
        Ok(settings)
    }

    pub fn next_gamma(&mut self) {
        self.gamma = [0.0, 0.5, 0.75, 1.0, 1.1, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0]
            .into_iter()
            .find(|value| *value > self.gamma)
            .unwrap_or(0.0);
    }

    pub fn next_cutoff(&mut self) {
        self.cutoff = [0.0, 0.02, 0.05, 0.1, 0.15, 0.2, 0.3]
            .into_iter()
            .find(|value| *value > self.cutoff)
            .unwrap_or(0.0);
    }
}

#[cfg(test)]
mod tests;
