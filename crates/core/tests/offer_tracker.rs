//! `WlOfferTracker`'s public contract: offers by id, the two selections, and slot reuse.

use clipleaf_core::WlOfferTracker;

const A: u32 = 0xff00_0000;
const B: u32 = 0xff00_0001;

#[test]
fn types_go_to_their_own_offer_and_selections_point_at_offers() {
    let mut tracker = WlOfferTracker::new();
    tracker.data_device_offer(A).unwrap();
    tracker.data_device_offer(B).unwrap();
    tracker.data_control_offer(A, b"text/plain").unwrap();
    tracker.data_control_offer(B, b"image/png").unwrap();
    assert_eq!(tracker.selection(&Some(A)).unwrap(), None);
    assert_eq!(tracker.primary_selection(&Some(B)).unwrap(), None);

    let clipboard = tracker.get_selected_slot().unwrap();
    assert_eq!(clipboard.id(), A);
    assert_eq!(clipboard.offered_mime_types(), ["text/plain"]);
    let primary = tracker.get_primary_selected_slot().unwrap();
    assert_eq!(primary.id(), B);
    assert_eq!(primary.offered_mime_types(), ["image/png"]);
}

#[test]
fn a_new_selection_returns_the_previous_one_and_clearing_returns_it_too() {
    let mut tracker = WlOfferTracker::new();
    tracker.data_device_offer(A).unwrap();
    tracker.data_device_offer(B).unwrap();
    assert_eq!(tracker.selection(&Some(A)).unwrap(), None);
    assert_eq!(tracker.selection(&Some(B)).unwrap(), Some(A));
    assert_eq!(tracker.selection(&None).unwrap(), Some(B));
    assert!(tracker.get_selected_slot().is_none());
}

#[test]
fn removed_offers_free_their_slot_and_are_no_longer_selected() {
    let mut tracker = WlOfferTracker::new();
    for i in 0..8 {
        tracker.data_device_offer(A + i).unwrap();
    }
    assert!(tracker.data_device_offer(A + 8).is_err(), "all slots used");

    tracker.selection(&Some(A)).unwrap();
    tracker.remove(A);
    assert!(tracker.get_selected_slot().is_none());
    tracker.data_device_offer(A + 8).unwrap();
}
