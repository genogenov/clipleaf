use crate::{
    fd_io::InFdBuffer,
    wl::{
        objects::{WlObject, wl_enum},
        wl_message_reader::WlMessageReader,
        wl_message_writer::WlMessageWriter,
    },
};

pub struct WlDataControlDevice {
    pub local_id: u32,
}
impl WlObject for WlDataControlDevice {
    type Ops = WlDataControlDeviceOps;
    type Events = WlDataControlDeviceEvents;
}

wl_enum! {
    pub enum WlDataControlDeviceOps {
        SetSelection = 0,

        Destroy = 1,

        SetPrimarySelection = 2,
    }
}

const DATA_OFFER: u16 = 0;
const SELECTION: u16 = 1;
const FINISHED: u16 = 2;
const PRIMARY_SELECTION: u16 = 3;

wl_enum! {
    pub enum WlDataControlDeviceEvents {
        DataOffer = DATA_OFFER,
        Selection = SELECTION,
        Finished = FINISHED,
        PrimarySelection = PRIMARY_SELECTION,
    }
}

pub enum DataControlDeviceEvent {
    DataOffer { new_id: u32 },
    Selection { offer_id: Option<u32> },
    Finished,
    PrimarySelection { offer_id: Option<u32> },
}

impl WlDataControlDevice {
    pub fn parse_message(
        opcode: u16,
        buffer: &[u8],
        _fds: &mut InFdBuffer,
    ) -> std::io::Result<DataControlDeviceEvent> {
        let mut reader = WlMessageReader::new(buffer);
        match opcode {
            DATA_OFFER => {
                let new_id = reader.u32().ok_or_else(|| {
                    std::io::Error::new(std::io::ErrorKind::InvalidData, "Failed to read new_id")
                })?;
                Ok(DataControlDeviceEvent::DataOffer { new_id })
            }
            SELECTION => {
                let offer_id = reader.u32().ok_or_else(|| {
                    std::io::Error::new(std::io::ErrorKind::InvalidData, "Failed to read offer_id")
                })?;
                Ok(DataControlDeviceEvent::Selection {
                    offer_id: if offer_id != 0 { Some(offer_id) } else { None },
                })
            }
            FINISHED => Ok(DataControlDeviceEvent::Finished),
            PRIMARY_SELECTION => {
                let offer_id = reader.u32().ok_or_else(|| {
                    std::io::Error::new(std::io::ErrorKind::InvalidData, "Failed to read offer_id")
                })?;
                Ok(DataControlDeviceEvent::PrimarySelection {
                    offer_id: if offer_id != 0 { Some(offer_id) } else { None },
                })
            }
            _ => Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("Unknown opcode: {}", opcode),
            )),
        }
    }

    pub fn set_selection(&self, writer: WlMessageWriter, source_id: u32) -> std::io::Result<()> {
        let mut msg = writer.begin_message::<WlDataControlDevice>(
            WlDataControlDeviceOps::SetSelection,
            self.local_id,
        )?;
        msg.pack_u32(source_id)?;
        msg.end();
        Ok(())
    }

    pub fn set_primary_selection(
        &self,
        writer: WlMessageWriter,
        source_id: u32,
    ) -> std::io::Result<()> {
        let mut msg = writer.begin_message::<WlDataControlDevice>(
            WlDataControlDeviceOps::SetPrimarySelection,
            self.local_id,
        )?;
        msg.pack_u32(source_id)?;
        msg.end();
        Ok(())
    }
}
