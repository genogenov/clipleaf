use std::io;

use crate::WlDataControlOffer;

pub struct WlOfferTracker {
    offers: [Option<WlDataControlOffer>; 8], // Something higher than 2 as the compositor sends offers before selection.
    selection_id: Option<u32>,
    primary_selection_id: Option<u32>,
}

impl Default for WlOfferTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl WlOfferTracker {
    pub fn new() -> Self {
        Self {
            offers: [None, None, None, None, None, None, None, None],
            selection_id: None,
            primary_selection_id: None,
        }
    }

    pub fn get_selected_slot(&self) -> Option<&WlDataControlOffer> {
        if let Some(selection_id) = self.selection_id {
            self.offers.iter().flatten().find(|offer| offer.id() == selection_id)
        } else {
            None
        }
    }

    pub fn get_primary_selected_slot(&self) -> Option<&WlDataControlOffer> {
        if let Some(primary_selection_id) = self.primary_selection_id {
            self.offers.iter().flatten().find(|offer| offer.id() == primary_selection_id)
        } else {
            None
        }
    }

    pub fn data_device_offer(&mut self, new_id: u32) -> Result<(), std::io::Error> {
        // log_debug!(
        //     "Received DataControlDevice DataOffer with new_id {}",
        //     new_id
        // );

        let candidate = self.offers.iter_mut().find(|offer| offer.is_none());
        if let Some(slot) = candidate {
            *slot = Some(WlDataControlOffer::new(new_id));
        } else {
            return Err(io::Error::other("No available slot for new DataOffer"));
        }
        Ok(())
    }

    pub fn data_control_offer(&mut self, id: u32, mime: &[u8]) -> Result<(), std::io::Error> {
        // log_debug!(
        //     "Received DataControlOffer with id {} and mime_type {:?}",
        //     id,
        //     String::from_utf8_lossy(mime)
        // );

        let Some(offer) = self.offers.iter_mut().flatten().find(|offer| offer.id() == id) else {
            return Err(io::Error::new(io::ErrorKind::NotFound, "Offer not found"));
        };
        offer.push_offer(mime);
        Ok(())
    }

    pub fn selection(&mut self, offer_id: &Option<u32>) -> Result<Option<u32>, std::io::Error> {
        // log_debug!(
        //     "Received DataControlDevice Selection with offer_id {:?}",
        //     offer_id
        // );
        let previous_selection = self.selection_id;
        self.selection_id = *offer_id;

        Ok(previous_selection)
    }

    pub fn primary_selection(&mut self, offer_id: &Option<u32>) -> Result<Option<u32>, std::io::Error> {
        // log_debug!(
        //     "Received DataControlDevice PrimarySelection with offer_id {:?}",
        //     offer_id
        // );
        let previous_primary_selection = self.primary_selection_id;
        self.primary_selection_id = *offer_id;

        Ok(previous_primary_selection)
    }

    pub fn remove(&mut self, offer_id: u32) {
        if let Some(slot) = self.offers.iter_mut().find(|o| o.as_ref().is_some_and(|o| o.id() == offer_id)) {
            *slot = None;
        }
    }
}
