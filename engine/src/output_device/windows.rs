use std::sync::mpsc::{self, Receiver, SyncSender};
use std::thread::{self, JoinHandle};

use tokio::sync::mpsc::UnboundedSender;
use windows::Win32::Media::Audio::{
    EDataFlow, ERole, IMMDeviceEnumerator, IMMNotificationClient, IMMNotificationClient_Impl,
    MMDeviceEnumerator, eConsole, eRender,
};
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
};
use windows::Win32::UI::Shell::PropertiesSystem::PROPERTYKEY;
use windows::core::{PCWSTR, Result as WindowsResult, implement};

#[implement(IMMNotificationClient)]
struct NotificationClient {
    sender: UnboundedSender<()>,
}

#[allow(non_snake_case)]
impl IMMNotificationClient_Impl for NotificationClient {
    fn OnDeviceStateChanged(&self, _: &PCWSTR, _: u32) -> WindowsResult<()> {
        Ok(())
    }

    fn OnDeviceAdded(&self, _: &PCWSTR) -> WindowsResult<()> {
        Ok(())
    }

    fn OnDeviceRemoved(&self, _: &PCWSTR) -> WindowsResult<()> {
        Ok(())
    }

    fn OnDefaultDeviceChanged(
        &self,
        flow: EDataFlow,
        role: ERole,
        _: &PCWSTR,
    ) -> WindowsResult<()> {
        if flow == eRender && role == eConsole {
            let _ = self.sender.send(());
        }
        Ok(())
    }

    fn OnPropertyValueChanged(&self, _: &PCWSTR, _: &PROPERTYKEY) -> WindowsResult<()> {
        Ok(())
    }
}

pub struct Watcher {
    stop: SyncSender<()>,
    thread: Option<JoinHandle<()>>,
}

pub fn watch(sender: UnboundedSender<()>) -> Result<Watcher, String> {
    let (ready_tx, ready_rx) = mpsc::sync_channel(1);
    let (stop_tx, stop_rx) = mpsc::sync_channel(1);
    let thread = thread::Builder::new()
        .name("default-output-watcher".into())
        .spawn(move || run(sender, ready_tx, stop_rx))
        .map_err(|error| format!("failed to start default-output watcher thread: {error}"))?;

    match ready_rx.recv() {
        Ok(Ok(())) => Ok(Watcher {
            stop: stop_tx,
            thread: Some(thread),
        }),
        Ok(Err(error)) => {
            let _ = thread.join();
            Err(error)
        }
        Err(error) => {
            let _ = thread.join();
            Err(format!("default-output watcher stopped before registration: {error}"))
        }
    }
}

fn run(sender: UnboundedSender<()>, ready: SyncSender<Result<(), String>>, stop: Receiver<()>) {
    // Registration and release stay on the same dedicated MTA thread; the
    // native callback itself may arrive on any COM thread.
    if let Err(error) = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) } {
        let _ = ready.send(Err(format!("CoInitializeEx failed: {error}")));
        return;
    }

    let registration = (|| -> Result<(IMMDeviceEnumerator, IMMNotificationClient), String> {
        let enumerator: IMMDeviceEnumerator = unsafe {
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                .map_err(|error| format!("MMDeviceEnumerator activation failed: {error}"))?
        };
        let client: IMMNotificationClient = NotificationClient { sender }.into();
        unsafe { enumerator.RegisterEndpointNotificationCallback(&client) }
            .map_err(|error| format!("endpoint notification registration failed: {error}"))?;
        Ok((enumerator, client))
    })();

    match registration {
        Ok((enumerator, client)) => {
            if ready.send(Ok(())).is_ok() {
                let _ = stop.recv();
            }
            if let Err(error) = unsafe { enumerator.UnregisterEndpointNotificationCallback(&client) }
            {
                eprintln!("default-output notification unregistration failed: {error}");
            }
            drop(client);
            drop(enumerator);
        }
        Err(error) => {
            let _ = ready.send(Err(error));
        }
    }

    unsafe { CoUninitialize() };
}

impl Drop for Watcher {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Media::Audio::{eCapture, eCommunications, eMultimedia};

    #[test]
    fn only_default_console_render_changes_wake_engine() {
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let client = NotificationClient { sender };
        let endpoint = PCWSTR::null();

        client.OnDefaultDeviceChanged(eRender, eMultimedia, &endpoint).unwrap();
        client.OnDefaultDeviceChanged(eRender, eCommunications, &endpoint).unwrap();
        client.OnDefaultDeviceChanged(eCapture, eConsole, &endpoint).unwrap();
        client.OnDeviceAdded(&endpoint).unwrap();
        client.OnDeviceRemoved(&endpoint).unwrap();
        client.OnDeviceStateChanged(&endpoint, 1).unwrap();
        client.OnPropertyValueChanged(&endpoint, &PROPERTYKEY::default()).unwrap();
        assert!(receiver.try_recv().is_err());

        client.OnDefaultDeviceChanged(eRender, eConsole, &endpoint).unwrap();
        assert_eq!(receiver.try_recv(), Ok(()));
        assert!(receiver.try_recv().is_err());

        drop(receiver);
        client.OnDefaultDeviceChanged(eRender, eConsole, &endpoint).unwrap();
    }
}
