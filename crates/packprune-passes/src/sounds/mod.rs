use packprune_pass::Pass;

mod ogg;
mod stream;
mod targets;
mod transcode;

pub use targets::Targets;

#[cfg(test)]
mod tests;

pub fn passes() -> Vec<Box<dyn Pass>> {
    vec![
        Box::new(stream::StreamSounds),
        Box::new(transcode::Audio::Mono),
        Box::new(transcode::Audio::Rate),
        Box::new(transcode::Audio::Ogg),
        Box::new(transcode::Audio::Convert),
    ]
}
