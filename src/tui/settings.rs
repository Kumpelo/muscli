//! The settings view.
//!
//! Label, displayed value and behaviour used to be three arrays lined up by
//! index, in three different files, with nothing keeping them in step:
//! inserting a row anywhere but the end silently attached every later label to
//! the wrong setting. They are keyed by `SettingId` now, so a row carries its
//! own meaning and the order of the table is the only thing that decides
//! position.

use super::keys::SettingInput;
use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SettingId {
    ReplayGain,
    ReplayGainMode,
    TargetLufs,
    Resume,
    History,
    CompactDefault,
    ShowCovers,
    VolumeStep,
    AutoDiscover,
    CoverCache,
    Discord,
    Rescan,
    AnalyzeGain,
}

pub(super) struct SettingRow {
    pub(super) id: SettingId,
    pub(super) label: &'static str,
}

const fn row(id: SettingId, label: &'static str) -> SettingRow {
    SettingRow { id, label }
}

pub(super) const SETTINGS: [SettingRow; 13] = [
    row(SettingId::ReplayGain, "ReplayGain"),
    row(SettingId::ReplayGainMode, "Modo ReplayGain"),
    row(SettingId::TargetLufs, "Objetivo LUFS"),
    row(SettingId::Resume, "Restaurar posiciones"),
    row(SettingId::History, "Historial"),
    row(SettingId::CompactDefault, "Modo compacto por defecto"),
    row(SettingId::ShowCovers, "Mostrar portadas"),
    row(SettingId::VolumeStep, "Paso de volumen"),
    row(SettingId::AutoDiscover, "Autodetectar SD/USB"),
    row(SettingId::CoverCache, "Caché de portadas"),
    row(SettingId::Discord, "Discord Rich Presence"),
    row(SettingId::Rescan, "Reescanear biblioteca"),
    row(SettingId::AnalyzeGain, "Analizar ReplayGain"),
];

fn switch(enabled: bool) -> String {
    if enabled { "on" } else { "off" }.to_owned()
}

impl App {
    /// The value column for one row.
    pub(super) fn setting_value(&self, id: SettingId) -> String {
        match id {
            SettingId::ReplayGain => switch(self.config.replaygain_enabled),
            SettingId::ReplayGainMode => match self.config.replaygain_mode {
                ReplayGainMode::Album => "álbum".to_owned(),
                ReplayGainMode::Track => "pista".to_owned(),
            },
            SettingId::TargetLufs => format!("{:.0} LUFS", self.config.replaygain_target_lufs),
            SettingId::Resume => switch(self.config.resume_enabled),
            SettingId::History => switch(self.config.history_enabled),
            SettingId::CompactDefault => switch(self.config.compact_default),
            SettingId::ShowCovers => switch(self.config.show_covers),
            SettingId::VolumeStep => format!("{}%", self.config.volume_step),
            SettingId::AutoDiscover => switch(self.config.auto_discover_removable),
            SettingId::CoverCache => format!("{} MiB", self.config.cover_cache_mb),
            SettingId::Discord => switch(self.config.discord_enabled),
            SettingId::Rescan => if self.scan_running {
                "en curso"
            } else {
                "Enter/Space"
            }
            .to_owned(),
            SettingId::AnalyzeGain => self
                .gain_progress
                .map(|(done, total)| format!("{done}/{total}"))
                .unwrap_or_else(|| "Enter/Space".to_owned()),
        }
    }

    pub(super) fn adjust_setting(&mut self, input: SettingInput) -> Result<()> {
        let Some(id) = SETTINGS.get(self.selected).map(|row| row.id) else {
            return Ok(());
        };
        let increase = input.increases();
        let horizontal = input.is_horizontal();
        match id {
            SettingId::ReplayGain => {
                self.config.replaygain_enabled = !self.config.replaygain_enabled
            }
            SettingId::ReplayGainMode => {
                self.config.replaygain_mode = match self.config.replaygain_mode {
                    ReplayGainMode::Album => ReplayGainMode::Track,
                    ReplayGainMode::Track => ReplayGainMode::Album,
                }
            }
            SettingId::TargetLufs => {
                if !horizontal {
                    return Ok(());
                }
                self.config.replaygain_target_lufs = (self.config.replaygain_target_lufs
                    + if increase { 1.0 } else { -1.0 })
                .clamp(-30.0, -5.0)
            }
            SettingId::Resume => self.config.resume_enabled = !self.config.resume_enabled,
            SettingId::History => self.config.history_enabled = !self.config.history_enabled,
            SettingId::CompactDefault => self.config.compact_default = !self.config.compact_default,
            SettingId::ShowCovers => self.config.show_covers = !self.config.show_covers,
            SettingId::VolumeStep => {
                if !horizontal {
                    return Ok(());
                }
                self.config.volume_step = if increase {
                    self.config.volume_step.saturating_add(1).min(20)
                } else {
                    self.config.volume_step.saturating_sub(1).max(1)
                }
            }
            SettingId::AutoDiscover => {
                self.config.auto_discover_removable = !self.config.auto_discover_removable
            }
            SettingId::CoverCache => {
                if !horizontal {
                    return Ok(());
                }
                self.config.cover_cache_mb = if increase {
                    self.config.cover_cache_mb.saturating_add(8).min(512)
                } else {
                    self.config.cover_cache_mb.saturating_sub(8).max(16)
                };
            }
            SettingId::Discord => {
                if self.config.discord_application_id.is_none() {
                    self.status =
                        "Configura Discord con: muscli setup discord APPLICATION_ID".into();
                    return Ok(());
                }
                self.config.discord_enabled = !self.config.discord_enabled;
                self.discord = if self.config.discord_enabled {
                    self.config
                        .discord_application_id
                        .clone()
                        .map(|application_id| {
                            DiscordPresence::start(
                                application_id,
                                self.config.discord_large_image.clone(),
                            )
                        })
                } else {
                    None
                };
            }
            // Rows that run something rather than hold a value; horizontal
            // movement would otherwise trigger them by accident.
            SettingId::Rescan => {
                if horizontal {
                    return Ok(());
                }
                self.start_scan();
                self.status = "Reescaneo iniciado".into();
            }
            SettingId::AnalyzeGain => {
                if horizontal {
                    return Ok(());
                }
                self.start_gain_analysis()?;
            }
        }
        self.config.save(&self.paths)?;
        self.status = "Settings guardados".into();
        self.dirty = true;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_row_has_a_distinct_identity() {
        // The whole point of the ids: two rows sharing one would put the same
        // value and behaviour behind two different labels.
        for (index, row) in SETTINGS.iter().enumerate() {
            assert!(
                !SETTINGS[..index].iter().any(|other| other.id == row.id),
                "{} repeats an id already used above it",
                row.label
            );
        }
    }
}
