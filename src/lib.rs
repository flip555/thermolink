//! ThermoLink — a modular thermal-camera relay.
//!
//! Layers:
//!
//! * [`camera`] — the vendor-neutral driver contract ([`camera::CameraDriver`],
//!   [`camera::CameraSession`]) plus one implementation per camera family.
//! * [`stream`] — fan-out of decoded frames to any number of consumers
//!   (the web server, a recorder, the alarm engine).
//! * [`config`] — runtime configuration, identical in shape for every driver.
//!
//! The web layer never sees a camera protocol. It asks a driver for a
//! [`camera::CameraSpec`] and then consumes [`camera::CameraFrame`]s.

pub mod camera;
pub mod config;
pub mod stream;

pub use camera::{
    Calibration, CameraDriver, CameraFrame, CameraSession, CameraSpec, FramePayload, Peak,
    StreamKind, ThermalImage, ThermalStats,
};
