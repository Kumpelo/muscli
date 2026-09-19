//! The settings view.
//!
//! Label, displayed value and behaviour used to be three arrays lined up by
//! index, in three different files, with nothing keeping them in step:
//! inserting a row anywhere but the end silently attached every later label to
//! the wrong setting. They are keyed by `SettingId` now, so a row carries its
//! own meaning and the order of the table is the only thing that decides
//! position.
//!
//! The rows are grouped by what they affect — interface, then playback, then
//! the library, then integrations, with the two rows that run something at the
//! bottom.

use super::keys::SettingInput;
use super::*;
use crate::config::EQUALIZER_BANDS;
use crate::i18n::{self, LANGUAGE_CHOICES, Language};
use crate::t;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SettingId {
    Theme,
    Language,
    CompactDefault,
    ShowCovers,
    VolumeStep,
    Gapless,
    ReplayGain,
    ReplayGainMode,
    TargetLufs,
    /// One equaliser band, by its position in `EQUALIZER_BANDS`.
    EqualizerBand(usize),
    ResetEqualizer,
    Resume,
    History,
    AutoDiscover,
    ScanThreads,
    CoverCache,
    Discord,
    Rescan,
    AnalyzeGain,
}

pub(super) struct SettingRow {
    pub(super) id: SettingId,
    /// Translation key for the row's name.
    pub(super) label: &'static str,
}

const fn row(id: SettingId, label: &'static str) -> SettingRow {
    SettingRow { id, label }
}

const fn band(index: usize) -> SettingRow {
    row(SettingId::EqualizerBand(index), "setting.equalizer_band")
}

pub(super) const SETTINGS: [SettingRow; 26] = [
    row(SettingId::Theme, "setting.theme"),
    row(SettingId::Language, "setting.language"),
    row(SettingId::CompactDefault, "setting.compact_default"),
    row(SettingId::ShowCovers, "setting.show_covers"),
    row(SettingId::VolumeStep, "setting.volume_step"),
    row(SettingId::Gapless, "setting.gapless"),
    row(SettingId::ReplayGain, "setting.replaygain"),
    row(SettingId::ReplayGainMode, "setting.replaygain_mode"),
    row(SettingId::TargetLufs, "setting.target_lufs"),
    band(0),
    band(1),
    band(2),
    band(3),
    band(4),
    band(5),
    band(6),
    band(7),
    row(SettingId::ResetEqualizer, "setting.reset_equalizer"),
    row(SettingId::Resume, "setting.resume"),
    row(SettingId::History, "setting.history"),
    row(SettingId::AutoDiscover, "setting.auto_discover"),
    row(SettingId::ScanThreads, "setting.scan_threads"),
    row(SettingId::CoverCache, "setting.cover_cache"),
    row(SettingId::Discord, "setting.discord"),
    row(SettingId::Rescan, "setting.rescan"),
    row(SettingId::AnalyzeGain, "setting.analyze_gain"),
];

fn switch(enabled: bool) -> String {
    t!(if enabled {
        "setting.value.on"
    } else {
        "setting.value.off"
    })
    .to_owned()
}

/// A band's centre frequency, written the way an equaliser labels it.
fn frequency_label(hertz: u32) -> String {
    if hertz < 1_000 {
        return format!("{hertz} Hz");
    }
    let kilohertz = f64::from(hertz) / 1_000.0;
    if hertz.is_multiple_of(1_000) {
        format!("{kilohertz:.0} kHz")
    } else {
        format!("{kilohertz:.1} kHz")
    }
}

/// The next position in a list of choices, wrapping in either direction.
fn step(length: usize, position: usize, forward: bool) -> usize {
    if forward {
        (position + 1) % length
    } else {
        (position + length - 1) % length
    }
}

/// Where the configured language sits among the choices.
///
/// `es_ES.UTF-8` is the same choice as `es`, so a locale-shaped value is
/// matched by the language it names rather than by its exact spelling.
fn language_position(configured: &str) -> usize {
    let configured = configured.trim();
    LANGUAGE_CHOICES
        .iter()
        .position(|choice| choice.eq_ignore_ascii_case(configured))
        .or_else(|| {
            let language = Language::from_tag(configured)?;
            LANGUAGE_CHOICES
                .iter()
                .position(|choice| *choice == language.code())
        })
        .unwrap_or(0)
}

impl App {
    /// The name column for one row.
    ///
    /// Almost every row is a plain translated label; the equaliser bands carry
    /// their own frequency, which is why this is a method rather than the
    /// label key alone.
    pub(super) fn setting_label(&self, row: &SettingRow) -> String {
        match row.id {
            SettingId::EqualizerBand(index) => t!(
                row.label,
                value = frequency_label(EQUALIZER_BANDS[index.min(EQUALIZER_BANDS.len() - 1)])
            ),
            _ => t!(row.label).to_owned(),
        }
    }

    /// The value column for one row.
    pub(super) fn setting_value(&self, id: SettingId) -> String {
        match id {
            SettingId::Theme => t!(self.theme_choice().label()).to_owned(),
            SettingId::Language => {
                let configured = LANGUAGE_CHOICES[language_position(&self.config.language)];
                match Language::from_tag(configured) {
                    Some(language) => language.label().to_owned(),
                    // "auto": say which language that works out to, so the row
                    // is not the only one in the view without a visible value.
                    None => t!(
                        "setting.value.language_auto",
                        value = i18n::resolve_language(None, configured).label()
                    ),
                }
            }
            SettingId::CompactDefault => switch(self.config.compact_default),
            SettingId::ShowCovers => switch(self.config.show_covers),
            SettingId::VolumeStep => t!("setting.value.percent", value = self.config.volume_step),
            SettingId::Gapless => switch(self.config.gapless),
            SettingId::ReplayGain => switch(self.config.replaygain_enabled),
            SettingId::ReplayGainMode => t!(match self.config.replaygain_mode {
                ReplayGainMode::Album => "setting.value.album",
                ReplayGainMode::Track => "setting.value.track",
            })
            .to_owned(),
            SettingId::TargetLufs => t!(
                "setting.value.lufs",
                value = format!("{:.0}", self.config.replaygain_target_lufs)
            ),
            SettingId::EqualizerBand(index) => t!(
                "setting.value.decibels",
                value = format!("{:+.0}", self.config.equalizer_gain(index))
            ),
            SettingId::ResetEqualizer => t!("setting.value.activate").to_owned(),
            SettingId::Resume => switch(self.config.resume_enabled),
            SettingId::History => switch(self.config.history_enabled),
            SettingId::AutoDiscover => switch(self.config.auto_discover_removable),
            SettingId::ScanThreads => {
                if self.config.scan_threads == 0 {
                    t!("setting.value.automatic").to_owned()
                } else {
                    t!("setting.value.threads", value = self.config.scan_threads)
                }
            }
            SettingId::CoverCache => t!("setting.value.mib", value = self.config.cover_cache_mb),
            SettingId::Discord => switch(self.config.discord_enabled),
            SettingId::Rescan => t!(if self.scan_running {
                "setting.value.running"
            } else {
                "setting.value.activate"
            })
            .to_owned(),
            SettingId::AnalyzeGain => self
                .gain_progress
                .map(|(done, total)| t!("setting.value.progress", done = done, total = total))
                .unwrap_or_else(|| t!("setting.value.activate").to_owned()),
        }
    }

    pub(super) fn adjust_setting(&mut self, input: SettingInput) -> Result<()> {
        let Some(id) = SETTINGS.get(self.selected).map(|row| row.id) else {
            return Ok(());
        };
        let increase = input.increases();
        let horizontal = input.is_horizontal();
        match id {
            SettingId::Theme => {
                // Space and Enter step forward, the arrows step either way, so
                // the row can be cycled without remembering which key it wants.
                self.config.theme = self
                    .theme_choice()
                    .step(increase || !horizontal)
                    .name()
                    .to_owned();
                // Choosing here is the lasting choice, so a `--theme` given
                // for this run stops speaking for the interface.
                self.theme_override = None;
                self.apply_theme();
            }
            SettingId::Language => {
                let position = language_position(&self.config.language);
                let next = step(LANGUAGE_CHOICES.len(), position, increase || !horizontal);
                self.config.language = LANGUAGE_CHOICES[next].to_owned();
                i18n::set_language(i18n::resolve_language(None, &self.config.language));
            }
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
            SettingId::Gapless => self.config.gapless = !self.config.gapless,
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
            SettingId::EqualizerBand(index) => {
                if !horizontal {
                    return Ok(());
                }
                let gain = self.config.equalizer_gain(index) + if increase { 1.0 } else { -1.0 };
                self.config.set_equalizer_gain(index, gain);
                self.mpv.set_equalizer(&self.config.equalizer_bands())?;
            }
            SettingId::ResetEqualizer => {
                if horizontal {
                    return Ok(());
                }
                self.config.equalizer.clear();
                self.mpv.set_equalizer(&self.config.equalizer_bands())?;
            }
            SettingId::Resume => self.config.resume_enabled = !self.config.resume_enabled,
            SettingId::History => self.config.history_enabled = !self.config.history_enabled,
            SettingId::AutoDiscover => {
                self.config.auto_discover_removable = !self.config.auto_discover_removable
            }
            SettingId::ScanThreads => {
                if !horizontal {
                    return Ok(());
                }
                // Zero is not "no scanning"; it is the automatic setting, and
                // it is the bottom of the range rather than a value skipped
                // over on the way down.
                self.config.scan_threads = if increase {
                    self.config.scan_threads.saturating_add(1).min(32)
                } else {
                    self.config.scan_threads.saturating_sub(1)
                }
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
                self.config.discord_enabled = !self.config.discord_enabled;
                self.discord = if self.config.discord_enabled {
                    Some(DiscordPresence::start(
                        self.config.discord_large_image.clone(),
                    ))
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
                self.status = t!("status.rescan_started").into();
            }
            SettingId::AnalyzeGain => {
                if horizontal {
                    return Ok(());
                }
                self.start_gain_analysis()?;
            }
        }
        self.config.save(&self.paths)?;
        self.status = t!("status.settings_saved").into();
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

    #[test]
    fn every_equaliser_band_has_a_row_of_its_own() {
        for index in 0..EQUALIZER_BANDS.len() {
            assert!(
                SETTINGS
                    .iter()
                    .any(|row| row.id == SettingId::EqualizerBand(index)),
                "band {index} is not reachable from the settings view"
            );
        }
    }

    #[test]
    fn frequencies_read_the_way_an_equaliser_labels_them() {
        assert_eq!(frequency_label(60), "60 Hz");
        assert_eq!(frequency_label(1_000), "1 kHz");
        assert_eq!(frequency_label(2_400), "2.4 kHz");
        assert_eq!(frequency_label(16_000), "16 kHz");
    }

    #[test]
    fn a_locale_shaped_language_matches_the_choice_it_names() {
        assert_eq!(language_position("auto"), 0);
        assert_eq!(language_position("es"), 2);
        assert_eq!(language_position("es_ES.UTF-8"), 2);
        assert_eq!(language_position("en_GB"), 1);
        // Nothing muscli speaks: back to following the system.
        assert_eq!(language_position("fr"), 0);
    }

    #[test]
    fn stepping_a_list_of_choices_wraps() {
        assert_eq!(step(3, 0, false), 2);
        assert_eq!(step(3, 2, true), 0);
        assert_eq!(step(3, 0, true), 1);
    }
}
