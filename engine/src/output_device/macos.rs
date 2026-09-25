use std::ptr::NonNull;

use block2::RcBlock;
use dispatch2::{DispatchQueue, DispatchRetained};
use objc2_core_audio::{
    AudioObjectAddPropertyListenerBlock, AudioObjectPropertyAddress,
    AudioObjectRemovePropertyListenerBlock, kAudioHardwarePropertyDefaultOutputDevice,
    kAudioObjectPropertyElementMain, kAudioObjectPropertyScopeGlobal, kAudioObjectSystemObject,
};
use tokio::sync::mpsc::UnboundedSender;

static DEFAULT_OUTPUT: AudioObjectPropertyAddress = AudioObjectPropertyAddress {
    mSelector: kAudioHardwarePropertyDefaultOutputDevice,
    mScope: kAudioObjectPropertyScopeGlobal,
    mElement: kAudioObjectPropertyElementMain,
};

type ListenerBlock = RcBlock<dyn Fn(u32, NonNull<AudioObjectPropertyAddress>)>;

pub struct Watcher {
    queue: DispatchRetained<DispatchQueue>,
    block: ListenerBlock,
}

pub fn watch(sender: UnboundedSender<()>) -> Result<Watcher, String> {
    // CoreAudio retains its own copy of the block and this serial queue until
    // removal. The block owns the sender, so callbacks never borrow Watcher.
    let queue = DispatchQueue::new("renderer-engine.default-output", None);
    let block: ListenerBlock = RcBlock::new(
        move |count: u32, addresses: NonNull<AudioObjectPropertyAddress>| {
            // SAFETY: CoreAudio supplies `count` valid property addresses for the
            // duration of this callback, including a non-null pointer when empty.
            let changed = unsafe { std::slice::from_raw_parts(addresses.as_ptr(), count as usize) };
            if changed.iter().any(|address| *address == DEFAULT_OUTPUT) {
                let _ = sender.send(());
            }
        },
    );

    // SAFETY: Both the static address and the owned block are valid for this
    // call; CoreAudio copies the block and retains the queue on success.
    let status = unsafe {
        AudioObjectAddPropertyListenerBlock(
            kAudioObjectSystemObject as u32,
            NonNull::from(&DEFAULT_OUTPUT),
            Some(&queue),
            RcBlock::as_ptr(&block),
        )
    };
    if status != 0 {
        return Err(format!(
            "CoreAudio could not register default output listener (OSStatus {status})"
        ));
    }

    Ok(Watcher { queue, block })
}

impl Drop for Watcher {
    fn drop(&mut self) {
        // SAFETY: The exact same object, address, queue, and block used for
        // registration remain alive through this call. CoreAudio owns a copy
        // of the block while registered, including if removal fails.
        let status = unsafe {
            AudioObjectRemovePropertyListenerBlock(
                kAudioObjectSystemObject as u32,
                NonNull::from(&DEFAULT_OUTPUT),
                Some(&self.queue),
                RcBlock::as_ptr(&self.block),
            )
        };
        if status != 0 {
            eprintln!("CoreAudio could not remove default output listener (OSStatus {status})");
        }
        // Drain callbacks already scheduled on the serial queue before the
        // local block and queue are released. Never run this on the queue.
        self.queue.exec_sync(|| {});
    }
}
