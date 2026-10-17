//! Which types `paste` picks by default (`pick_best_mime`) and which ones the daemon fetches
//! (`pick_fetch_mimes`): the best text, the best image, and every file-list type.

use clipleaf_core::{pick_best_mime, pick_fetch_mimes};

fn fetch(offered: &[&'static str]) -> Vec<&'static str> {
    pick_fetch_mimes(offered, 10)
}

fn best(offered: &[&'static str]) -> Option<&'static str> {
    pick_best_mime(offered).copied()
}

#[test]
fn the_best_text_and_the_best_image_are_fetched_and_nothing_else() {
    let offered = [
        "application/x-qt-image",
        "image/jpeg",
        "text/html",
        "TEXT",
        "image/png",
        "text/plain;charset=utf-8",
        "UTF8_STRING",
        "chromium/x-source-url",
    ];
    assert_eq!(fetch(&offered), ["text/plain;charset=utf-8", "image/png"]);
}

#[test]
fn text_falls_back_to_html_then_other_text_then_utf16() {
    assert_eq!(fetch(&["text/x-moz-url", "text/html"]), ["text/html"]);
    assert_eq!(
        fetch(&["text/plain;charset=utf-16", "text/x-moz-url"]),
        ["text/x-moz-url"]
    );
    assert_eq!(
        fetch(&["application/octet-stream", "text/plain;charset=utf-16"]),
        ["text/plain;charset=utf-16"]
    );
}

#[test]
fn any_image_is_fetched_when_there_is_no_png() {
    assert_eq!(fetch(&["image/webp", "image/jpeg"]), ["image/webp"]);
}

#[test]
fn every_file_list_type_is_fetched_after_the_text_so_file_copies_restore() {
    // What a file manager offers (Nautilus / Dolphin), in their order.
    let offered = [
        "x-special/gnome-copied-files",
        "application/x-kde-cutselection",
        "text/uri-list",
        "application/x-kde4-urilist",
        "text/plain;charset=utf-8",
        "application/vnd.portal.filetransfer",
        "application/x-kde-source-id",
    ];
    assert_eq!(
        fetch(&offered),
        [
            "text/plain;charset=utf-8",
            "x-special/gnome-copied-files",
            "application/x-kde-cutselection",
            "text/uri-list",
            "application/x-kde4-urilist",
        ]
    );
}

#[test]
fn a_password_manager_copy_is_not_fetched_at_all() {
    assert!(fetch(&["text/plain;charset=utf-8", "x-kde-passwordManagerHint"]).is_empty());
}

#[test]
fn nothing_known_means_nothing_fetched_and_the_cap_is_honored() {
    assert!(fetch(&["application/x-foo", "chromium/x-web-custom-data"]).is_empty());
    assert!(fetch(&[]).is_empty());
    let offered = ["text/plain", "image/png", "text/uri-list"];
    assert_eq!(pick_fetch_mimes(&offered, 2), ["text/plain", "image/png"]);
}

#[test]
fn paste_picks_text_then_text_like_then_files_then_images_then_the_first_other() {
    assert_eq!(
        best(&["image/png", "text/html", "STRING", "UTF8_STRING"]),
        Some("UTF8_STRING")
    );
    assert_eq!(
        best(&["image/png", "text/plain;charset=utf-16", "text/html"]),
        Some("text/html")
    );
    assert_eq!(
        best(&["image/png", "text/plain;charset=utf-16"]),
        Some("text/plain;charset=utf-16")
    );
    assert_eq!(best(&["image/png", "text/uri-list"]), Some("text/uri-list"));
    assert_eq!(
        best(&["application/x-foo", "image/jpeg"]),
        Some("image/jpeg")
    );
    assert_eq!(
        best(&["application/x-b", "application/x-a"]),
        Some("application/x-b")
    );
    assert_eq!(best(&["x-kde-passwordManagerHint"]), None);
    assert_eq!(best(&[]), None);
}
