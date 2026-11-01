use std::{env, ffi::OsString, path::PathBuf};

use clipleaf_core::{log_debug, log_error};
use gtk4::gdk;

/// User stylesheet, relative to the XDG config dir.
const USER_CSS: &str = "clipleaf-default-gui/style.css";

/// Built-in CSS at APPLICATION priority, then the user's `style.css` (if it exists) at USER priority,
/// so any user rule wins over ours. Call from `startup` (needs an initialised display).
pub fn load(display: &gdk::Display) {
    add_provider(display, gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION, |css| {
        css.load_from_string(include_str!("../assets/default.css"));
    });

    let candidates = user_css_candidates(env::var_os("XDG_CONFIG_HOME"), env::var_os("HOME"));
    let Some(path) = candidates.into_iter().find(|p| p.is_file()) else {
        log_debug!("No user CSS found");
        return;
    };
    log_debug!("Loading user CSS from {}", path.display());
    add_provider(display, gtk4::STYLE_PROVIDER_PRIORITY_USER, |css| css.load_from_path(&path));
}

fn add_provider(display: &gdk::Display, priority: u32, load: impl FnOnce(&gtk4::CssProvider)) {
    let css = gtk4::CssProvider::new();
    // Before loading: a typo becomes a log line with file:line:col instead of a silently ignored rule.
    css.connect_parsing_error(|_, section, err| log_error!("CSS {}: {}", section.to_str(), err));
    load(&css);
    gtk4::style_context_add_provider_for_display(display, &css, priority);
}

/// Where to look for the user CSS, in order: `$XDG_CONFIG_HOME/<USER_CSS>`, then `$HOME/.config/<USER_CSS>`.
/// The first one that exists is used. Unset, empty or relative values are skipped (the XDG spec says to
/// ignore relative paths).
fn user_css_candidates(xdg_config_home: Option<OsString>, home: Option<OsString>) -> Vec<PathBuf> {
    let xdg = xdg_config_home.map(PathBuf::from).filter(|p| p.is_absolute());
    let home = home.map(PathBuf::from).filter(|p| p.is_absolute()).map(|h| h.join(".config"));
    [xdg, home].into_iter().flatten().map(|dir| dir.join(USER_CSS)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn some(s: &str) -> Option<OsString> {
        Some(OsString::from(s))
    }

    #[test]
    fn user_css_candidates_xdg_first_then_home_config() {
        assert_eq!(
            user_css_candidates(some("/xdg"), some("/home/u")),
            [
                PathBuf::from("/xdg/clipleaf-default-gui/style.css"),
                PathBuf::from("/home/u/.config/clipleaf-default-gui/style.css"),
            ]
        );
    }

    #[test]
    fn user_css_candidates_skip_unset_empty_and_relative() {
        let home_only = [PathBuf::from("/home/u/.config/clipleaf-default-gui/style.css")];
        assert_eq!(user_css_candidates(None, some("/home/u")), home_only);
        assert_eq!(user_css_candidates(some(""), some("/home/u")), home_only);
        assert_eq!(user_css_candidates(some("rel/xdg"), some("/home/u")), home_only);
        assert_eq!(
            user_css_candidates(some("/xdg"), some("home")),
            [PathBuf::from("/xdg/clipleaf-default-gui/style.css")]
        );
        assert!(user_css_candidates(None, None).is_empty());
    }
}
