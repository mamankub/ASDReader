use super::{PrintTicket, DEFAULT_PRINT_TICKET_XML};
use crate::{
    printer::PrinterDevice,
    utils::{stream::copy_com_stream_to_vec, wchar},
};
use thiserror::Error;
use scopeguard::defer;
use std::ptr;
use windows::{
    core::{BSTR, PCWSTR},
    Win32::{
        Graphics::Printing::PrintTicket::{
            kPTJobScope, kUserDefaultDevmode, PTCloseProvider, PTConvertDevModeToPrintTicket,
            PTConvertPrintTicketToDevMode, PTMergeAndValidatePrintTicket, PTOpenProvider,
            PTReleaseMemory, HPTPROVIDER,
        },
        UI::Shell::SHCreateMemStream,
    },
};

/// Represents a print ticket builder, which can be used to build a print ticket.
pub struct PrintTicketBuilder {
    xml: Vec<u8>,
    provider: HPTPROVIDER,
}

#[derive(Error, Debug)]
/// Represents an error occurred while building a print ticket.
pub enum PrintTicketBuilderError {
    /// Failed to open print ticket provider.
    #[error("Failed to open print ticket provider")]
    OpenProviderFailed(#[source] windows::core::Error),
    /// Stream not allocated.
    #[error("Stream not allocated")]
    StreamNotAllocated,
    /// Failed to merge print tickets.
    #[error("Failed to merge print tickets")]
    MergePrintTicketsFailed(String, #[source] windows::core::Error),
    /// Failed to decode print ticket.
    #[error("Failed to decode print ticket")]
    DecodePrintTicketFailed(#[source] windows::core::Error),
}

/// ASDReader patch: the printer's settings for the current user (what Printing Preferences
/// shows: resolution, print quality, paper type, input tray, duplex and so on), as a PrintTicket.
///
/// A builder used to start from an empty ticket, so a job carried only what the caller merged in
/// and everything else came from the driver's own defaults. Whatever the user had set for the
/// printer, a 1200 dpi resolution included, never reached the job. The user default DEVMODE is
/// what Printing Preferences stores, and the Print Ticket provider converts it both ways.
///
/// `None` when any step fails; the caller then starts from the empty ticket as before.
fn user_default_ticket(provider: HPTPROVIDER) -> Option<Vec<u8>> {
    unsafe {
        let empty = SHCreateMemStream(Some(DEFAULT_PRINT_TICKET_XML.as_bytes()))?;
        let mut dev_mode_size = 0u32;
        let mut dev_mode = ptr::null_mut();
        PTConvertPrintTicketToDevMode(
            provider,
            &empty,
            kUserDefaultDevmode,
            kPTJobScope,
            ptr::addr_of_mut!(dev_mode_size),
            ptr::addr_of_mut!(dev_mode),
            None,
        )
        .ok()?;
        defer! {
            let _ = PTReleaseMemory(dev_mode as *const _);
        }
        if dev_mode.is_null() || dev_mode_size == 0 {
            return None;
        }
        let ticket = SHCreateMemStream(None)?;
        PTConvertDevModeToPrintTicket(
            provider,
            dev_mode_size,
            dev_mode as *const _,
            kPTJobScope,
            &ticket,
        )
        .ok()?;
        let mut xml = Vec::new();
        copy_com_stream_to_vec(&mut xml, &ticket).ok()?;
        usable_ticket(xml)
    }
}

/// ASDReader patch: a converted ticket is used only when it is a PrintTicket document, so a
/// driver that answers with nothing (or with something else) leaves the builder on the empty ticket.
fn usable_ticket(xml: Vec<u8>) -> Option<Vec<u8>> {
    let text = std::str::from_utf8(&xml).ok()?;
    (text.contains("PrintTicket") && text.contains("printschemaframework")).then_some(xml)
}

impl PrintTicketBuilder {
    /// Create a new print ticket builder for the given printer device.
    ///
    /// ASDReader patch: it starts from the printer's settings for the current user
    /// ([`user_default_ticket`]), not from an empty ticket, so what a job does not set follows
    /// Printing Preferences, including the print resolution.
    pub fn new(device: &PrinterDevice) -> Result<Self, PrintTicketBuilderError> {
        let provider = unsafe {
            PTOpenProvider(PCWSTR(wchar::to_wide_chars(device.os_name()).as_ptr()), 1)
                .map_err(PrintTicketBuilderError::OpenProviderFailed)?
        };
        let xml = user_default_ticket(provider).unwrap_or_else(|| DEFAULT_PRINT_TICKET_XML.into());
        Ok(Self { xml, provider })
    }

    /// Merge the given print ticket into the current print ticket.
    pub fn merge(&mut self, delta: impl Into<PrintTicket>) -> Result<(), PrintTicketBuilderError> {
        unsafe {
            let base = SHCreateMemStream(Some(self.xml.as_ref()))
                .ok_or(PrintTicketBuilderError::StreamNotAllocated)?;
            let delta = SHCreateMemStream(Some(delta.into().get_xml()))
                .ok_or(PrintTicketBuilderError::StreamNotAllocated)?;
            let result =
                SHCreateMemStream(None).ok_or(PrintTicketBuilderError::StreamNotAllocated)?;
            let mut error_message = BSTR::default();
            PTMergeAndValidatePrintTicket(
                self.provider,
                &base,
                &delta,
                kPTJobScope,
                Some(&result),
                Some(&mut error_message),
            )
            .map_err(|win32_error| {
                PrintTicketBuilderError::MergePrintTicketsFailed(
                    error_message.to_string(),
                    win32_error,
                )
            })?;
            copy_com_stream_to_vec(&mut self.xml, &result)
                .map_err(PrintTicketBuilderError::DecodePrintTicketFailed)?;
        }
        Ok(())
    }

    /// Build the print ticket.
    pub fn build(mut self) -> Result<PrintTicket, PrintTicketBuilderError> {
        let xml = std::mem::take(&mut self.xml);
        Ok(PrintTicket { xml })
    }
}

impl Drop for PrintTicketBuilder {
    fn drop(&mut self) {
        unsafe {
            let _ = PTCloseProvider(self.provider);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::PrintTicketBuilder;
    use crate::{
        test_utils::null_device,
        ticket::{PrintTicket, PrintTicketBuilderError},
    };

    #[test]
    fn merge_simple_ticket() {
        let device = null_device::thread_local();
        let mut builder = PrintTicketBuilder::new(&device).unwrap();
        let delta = r#"<psf:PrintTicket xmlns:psf="http://schemas.microsoft.com/windows/2003/08/printing/printschemaframework" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" xmlns:xsd="http://www.w3.org/2001/XMLSchema" version="1" xmlns:psk="http://schemas.microsoft.com/windows/2003/08/printing/printschemakeywords">
    <psf:Feature name="psk:PageMediaSize">
		<psf:Option name="psk:NorthAmericaTabloid">
			<psf:ScoredProperty name="psk:MediaSizeWidth">
				<psf:Value xsi:type="xsd:integer">279400</psf:Value>
			</psf:ScoredProperty>
			<psf:ScoredProperty name="psk:MediaSizeHeight">
				<psf:Value xsi:type="xsd:integer">431800</psf:Value>
			</psf:ScoredProperty>
			<psf:Property name="psk:DisplayName">
				<psf:Value xsi:type="xsd:string">Tabloid</psf:Value>
			</psf:Property>
		</psf:Option>
	</psf:Feature>
</psf:PrintTicket>"#;
        builder.merge(PrintTicket::from_xml(delta)).unwrap();
        let _ticket = builder.build().unwrap();
    }

    #[test]
    fn merge_invalid_ticket() {
        let device = null_device::thread_local();
        let mut builder = PrintTicketBuilder::new(&device).unwrap();
        let delta = r#"This is not a valid print ticket"#;
        let result = builder.merge(PrintTicket::from_xml(delta));
        assert!(matches!(
            result,
            Err(PrintTicketBuilderError::MergePrintTicketsFailed(..))
        ));
    }
}

#[cfg(test)]
mod user_default_tests {
    use super::{usable_ticket, DEFAULT_PRINT_TICKET_XML};

    #[test]
    fn only_a_print_ticket_document_replaces_the_empty_ticket() {
        let converted = br#"<psf:PrintTicket xmlns:psf="http://schemas.microsoft.com/windows/2003/08/printing/printschemaframework" version="1"><psf:Feature name="psk:PageResolution"/></psf:PrintTicket>"#;
        assert_eq!(usable_ticket(converted.to_vec()).as_deref(), Some(converted.as_slice()));
        assert!(usable_ticket(DEFAULT_PRINT_TICKET_XML.as_bytes().to_vec()).is_some());
        assert_eq!(usable_ticket(Vec::new()), None);
        assert_eq!(usable_ticket(b"<html></html>".to_vec()), None);
        assert_eq!(usable_ticket(vec![0xFF, 0xFE, 0x00]), None);
    }
}
