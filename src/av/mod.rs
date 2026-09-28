//! Audio, video and Lottie decoding/playback (ffmpeg, cpal, rlottie).
#![allow(dead_code)]

pub mod audio;
pub mod lottie;
pub mod player;
pub mod video;

#[cfg(test)]
mod tests;
