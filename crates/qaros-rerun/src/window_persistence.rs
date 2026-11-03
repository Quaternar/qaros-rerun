//! The desktop window's saved geometry (ADR-0103). eframe saves the window's position and size
//! under the `window` key at every autosave (`eframe` 0.37.0 `epi_integration.rs` L470-L489,
//! `STORAGE_WINDOW_KEY`), read from the window as it is at that moment
//! (`egui-winit` 0.37.0 `window_settings.rs` `WindowGeometry::from_window`), and a minimised
//! Windows window is parked at -32000,-32000 with a strip of a client area. A full pass of a
//! minimised window reaches the autosave (ADR-0097), so without this the next start restores a
//! window too small to find.

use rerun::external::eframe::Storage;

/// eframe's storage key for the window settings (`epi_integration.rs`, `STORAGE_WINDOW_KEY`).
const WINDOW_SETTINGS_KEY: &str = "window";

/// Rerun's own least window size (`re_viewer` 0.40.0 `native.rs` L46, `with_min_inner_size`):
/// a first shown window smaller than this was restored from a geometry nobody chose.
const SMALLEST_USABLE_WINDOW_POINTS: egui::Vec2 = egui::vec2(320.0, 450.0);

/// Rerun's own first window size (`re_viewer` 0.40.0 `native.rs` L45), kept within the monitor.
const DEFAULT_WINDOW_POINTS: egui::Vec2 = egui::vec2(1600.0, 1200.0);

/// The share of the monitor a restored-too-small window may take.
const MONITOR_SHARE_FOR_DEFAULT_WINDOW: f32 = 0.9;

/// Where a restored-too-small window is placed, from the monitor's top left corner.
const DEFAULT_WINDOW_OFFSET_POINTS: egui::Pos2 = egui::pos2(40.0, 40.0);

#[derive(Debug, Default)]
pub struct WindowPersistence {
    is_minimised: bool,
    last_shown_window_settings: Option<String>,
    first_shown_pass_checked: bool,
}

impl WindowPersistence {
    /// eframe's `App::raw_input_hook`, before every egui pass: notes whether the window is
    /// minimised, and gives the first shown window of the run a usable size when it was
    /// restored smaller than Rerun allows.
    pub fn observe_pass(&mut self, ctx: &egui::Context, raw_input: &egui::RawInput) {
        let viewport = raw_input.viewports.get(&raw_input.viewport_id);
        self.is_minimised = viewport.is_some_and(crate::hidden_window::is_minimised);
        if self.is_minimised || self.first_shown_pass_checked {
            return;
        }
        let Some(screen) = raw_input.screen_rect else {
            return;
        };
        self.first_shown_pass_checked = true;
        if !is_too_small_to_use(screen.size()) {
            return;
        }
        let monitor_size = viewport.and_then(|viewport| viewport.monitor_size);
        let default_size = default_window_size(monitor_size);
        re_log::info!(
            "headset: the window was restored at {:.0}x{:.0} points, smaller than Rerun's \
             {:.0}x{:.0}; it opens at {:.0}x{:.0}",
            screen.width(),
            screen.height(),
            SMALLEST_USABLE_WINDOW_POINTS.x,
            SMALLEST_USABLE_WINDOW_POINTS.y,
            default_size.x,
            default_size.y
        );
        ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(
            DEFAULT_WINDOW_OFFSET_POINTS,
        ));
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(default_size));
    }

    /// After eframe and the Rerun app have saved: a minimised window's geometry is replaced
    /// by the one saved while the window was last shown, or left out when there is none, so
    /// the next start opens at a size and place of use.
    pub fn keep_shown_window_settings(&mut self, storage: &mut dyn Storage) {
        if !self.is_minimised {
            self.last_shown_window_settings = storage.get_string(WINDOW_SETTINGS_KEY);
            return;
        }
        match &self.last_shown_window_settings {
            Some(shown_window_settings) => {
                storage.set_string(WINDOW_SETTINGS_KEY, shown_window_settings.clone());
            }
            None => storage.remove_string(WINDOW_SETTINGS_KEY),
        }
    }
}

fn is_too_small_to_use(window_points: egui::Vec2) -> bool {
    window_points.x < SMALLEST_USABLE_WINDOW_POINTS.x
        || window_points.y < SMALLEST_USABLE_WINDOW_POINTS.y
}

fn default_window_size(monitor_points: Option<egui::Vec2>) -> egui::Vec2 {
    monitor_points
        .filter(|monitor| monitor.x > 1.0 && monitor.y > 1.0)
        .map_or(DEFAULT_WINDOW_POINTS, |monitor| {
            DEFAULT_WINDOW_POINTS.min(monitor * MONITOR_SHARE_FOR_DEFAULT_WINDOW)
        })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    const SHOWN_SETTINGS: &str = "(normal_geometry:(outer_position_pixels:Some((x:120.0,y:120.0)),scale_factor:1.5,inner_size_points:(x:1167.0,y:757.0)),fullscreen:false,maximized:false)";
    const MINIMISED_SETTINGS: &str = "(normal_geometry:(outer_position_pixels:Some((x:-32000.0,y:-32000.0)),scale_factor:1.5,inner_size_points:(x:158.0,y:26.0)),fullscreen:false,maximized:false)";

    #[derive(Default)]
    struct MemoryStorage {
        values: HashMap<String, String>,
    }

    impl Storage for MemoryStorage {
        fn get_string(&self, key: &str) -> Option<String> {
            self.values.get(key).cloned()
        }

        fn set_string(&mut self, key: &str, value: String) {
            self.values.insert(key.to_owned(), value);
        }

        fn remove_string(&mut self, key: &str) {
            self.values.remove(key);
        }

        fn flush(&mut self) {}
    }

    fn pass_input(
        screen: egui::Vec2,
        minimised: bool,
        monitor: Option<egui::Vec2>,
    ) -> egui::RawInput {
        let mut raw_input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, screen)),
            ..Default::default()
        };
        raw_input.viewports.insert(
            raw_input.viewport_id,
            egui::ViewportInfo {
                minimized: Some(minimised),
                monitor_size: monitor,
                ..Default::default()
            },
        );
        raw_input
    }

    /// What eframe writes at an autosave, then what this run's save leaves in the storage.
    fn autosave(
        persistence: &mut WindowPersistence,
        storage: &mut MemoryStorage,
        window_as_eframe_reads_it: &str,
    ) -> Option<String> {
        storage.set_string(WINDOW_SETTINGS_KEY, window_as_eframe_reads_it.to_owned());
        persistence.keep_shown_window_settings(storage);
        storage.get_string(WINDOW_SETTINGS_KEY)
    }

    #[test]
    fn a_minimised_window_keeps_the_settings_saved_while_it_was_shown() {
        let ctx = egui::Context::default();
        let mut persistence = WindowPersistence::default();
        let mut storage = MemoryStorage::default();

        persistence.observe_pass(&ctx, &pass_input(egui::vec2(1167.0, 757.0), false, None));
        assert_eq!(
            autosave(&mut persistence, &mut storage, SHOWN_SETTINGS).as_deref(),
            Some(SHOWN_SETTINGS)
        );

        persistence.observe_pass(&ctx, &pass_input(egui::vec2(158.0, 26.0), true, None));
        assert_eq!(
            autosave(&mut persistence, &mut storage, MINIMISED_SETTINGS).as_deref(),
            Some(SHOWN_SETTINGS)
        );
    }

    #[test]
    fn a_window_minimised_before_any_shown_save_leaves_no_settings() {
        let ctx = egui::Context::default();
        let mut persistence = WindowPersistence::default();
        let mut storage = MemoryStorage::default();

        persistence.observe_pass(&ctx, &pass_input(egui::vec2(158.0, 26.0), true, None));
        assert_eq!(
            autosave(&mut persistence, &mut storage, MINIMISED_SETTINGS),
            None
        );
    }

    #[test]
    fn a_shown_window_saves_what_eframe_read_after_a_restore() {
        let ctx = egui::Context::default();
        let mut persistence = WindowPersistence::default();
        let mut storage = MemoryStorage::default();
        let moved_settings = SHOWN_SETTINGS.replace("x:120.0,y:120.0", "x:300.0,y:200.0");

        persistence.observe_pass(&ctx, &pass_input(egui::vec2(158.0, 26.0), true, None));
        persistence.observe_pass(&ctx, &pass_input(egui::vec2(1167.0, 757.0), false, None));
        assert_eq!(
            autosave(&mut persistence, &mut storage, &moved_settings),
            Some(moved_settings)
        );
    }

    /// The window geometry commands of one pass run after the hook, as eframe runs them.
    fn geometry_commands_of_pass(
        ctx: &egui::Context,
        persistence: &mut WindowPersistence,
        raw_input: egui::RawInput,
    ) -> Vec<egui::ViewportCommand> {
        persistence.observe_pass(ctx, &raw_input);
        let mut output = ctx.run_ui(raw_input, |_ui| {});
        output.textures_delta.clear();
        output.viewport_output[&egui::ViewportId::ROOT]
            .commands
            .iter()
            .filter(|command| {
                matches!(
                    command,
                    egui::ViewportCommand::OuterPosition(_) | egui::ViewportCommand::InnerSize(_)
                )
            })
            .cloned()
            .collect()
    }

    #[test]
    fn a_window_restored_smaller_than_rerun_allows_opens_at_the_default_size_once() {
        let ctx = egui::Context::default();
        let mut persistence = WindowPersistence::default();
        let laptop_monitor = Some(egui::vec2(1706.0, 1066.0));
        let nine_tenths_of_the_laptop_monitor = egui::vec2(1706.0 * 0.9, 1066.0 * 0.9);

        let first = pass_input(egui::vec2(158.0, 64.0), false, laptop_monitor);
        assert_eq!(
            geometry_commands_of_pass(&ctx, &mut persistence, first),
            vec![
                egui::ViewportCommand::OuterPosition(egui::pos2(40.0, 40.0)),
                egui::ViewportCommand::InnerSize(nine_tenths_of_the_laptop_monitor),
            ]
        );

        let second = pass_input(egui::vec2(158.0, 64.0), false, laptop_monitor);
        assert_eq!(
            geometry_commands_of_pass(&ctx, &mut persistence, second),
            vec![]
        );
    }

    #[test]
    fn a_minimised_first_pass_waits_for_the_window_to_be_shown() {
        let ctx = egui::Context::default();
        let mut persistence = WindowPersistence::default();
        let laptop_monitor = Some(egui::vec2(1706.0, 1066.0));
        let nine_tenths_of_the_laptop_monitor = egui::vec2(1706.0 * 0.9, 1066.0 * 0.9);

        let minimised = pass_input(egui::vec2(158.0, 26.0), true, laptop_monitor);
        assert_eq!(
            geometry_commands_of_pass(&ctx, &mut persistence, minimised),
            vec![]
        );

        let shown = pass_input(egui::vec2(158.0, 64.0), false, laptop_monitor);
        assert_eq!(
            geometry_commands_of_pass(&ctx, &mut persistence, shown),
            vec![
                egui::ViewportCommand::OuterPosition(egui::pos2(40.0, 40.0)),
                egui::ViewportCommand::InnerSize(nine_tenths_of_the_laptop_monitor),
            ]
        );
    }

    #[test]
    fn a_usable_first_window_is_left_as_restored() {
        let ctx = egui::Context::default();
        let mut persistence = WindowPersistence::default();
        let first = pass_input(
            egui::vec2(1167.0, 757.0),
            false,
            Some(egui::vec2(1706.0, 1066.0)),
        );
        assert_eq!(
            geometry_commands_of_pass(&ctx, &mut persistence, first),
            vec![]
        );
    }

    #[test]
    fn the_default_size_is_rerun_s_within_a_large_monitor_and_without_one() {
        assert_eq!(
            default_window_size(Some(egui::vec2(3840.0, 2160.0))),
            egui::vec2(1600.0, 1200.0)
        );
        assert_eq!(default_window_size(None), egui::vec2(1600.0, 1200.0));
        assert_eq!(
            default_window_size(Some(egui::vec2(1280.0, 800.0))),
            egui::vec2(1152.0, 720.0)
        );
    }

    #[test]
    fn too_small_means_under_rerun_s_least_size_on_either_side() {
        assert!(is_too_small_to_use(egui::vec2(158.0, 64.0)));
        assert!(is_too_small_to_use(egui::vec2(1600.0, 449.0)));
        assert!(!is_too_small_to_use(egui::vec2(320.0, 450.0)));
    }
}
