use chrono::prelude::{DateTime, Utc};
use clap::Parser;
use gilrs::GilrsBuilder;
use schemars::{schema_for, JsonSchema};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::hash::Hash;
use std::time::Duration;
use thiserror::Error;
use tracing::*;
use zenoh::config::Config;
use zenoh::prelude::r#async::*;

/// Visualize Hopper
#[derive(Parser)]
#[command(author, version)]
struct Args {
    /// The key expression to publish onto.
    #[clap(short, long, default_value = "remote-control/gamepad")]
    topic: String,

    /// Endpoints to connect to.
    #[clap(short = 'e', long)]
    connect: Vec<zenoh_config::EndPoint>,

    /// Endpoints to listen on.
    #[clap(long)]
    listen: Vec<zenoh_config::EndPoint>,

    /// A configuration file.
    #[clap(short, long)]
    config: Option<String>,

    /// Loop sleep time
    #[clap(short, long, default_value = "50")]
    sleep_ms: u64,
}

#[tokio::main(worker_threads = 2)]
async fn main() -> anyhow::Result<()> {
    let args: Args = Args::parse();
    setup_tracing(1);

    let mut zenoh_config = if let Some(conf_file) = &args.config {
        Config::from_file(conf_file).unwrap()
    } else {
        Config::default()
    };

    if !args.connect.is_empty() {
        zenoh_config.connect.endpoints = args.connect.clone();
    }
    if !args.listen.is_empty() {
        zenoh_config.listen.endpoints = args.listen.clone();
    }
    info!("Using config {:?}", args.config);
    info!("Connecting to {:?}", zenoh_config.connect.endpoints);
    info!("Listening on {:?}", zenoh_config.listen.endpoints);
    info!("Publishing on {:?}", args.topic);
    info!("Starting zenoh session");
    let zenoh_session = zenoh::open(zenoh_config)
        .res()
        .await
        .map_err(HopperRemoteError::ZenohError)?
        .into_arc();

    let gamepad_publisher = zenoh_session
        .declare_publisher(args.topic)
        .res()
        .await
        .map_err(HopperRemoteError::ZenohError)?;

    let schema = schema_for!(InputMessage);
    info!(
        "Message schema:\n{}",
        serde_json::to_string(&schema).unwrap()
    );

    info!("Starting gamepad reader");

    // gamepad
    let mut gilrs = GilrsBuilder::new()
        .with_default_filters(true)
        .build()
        .expect("Failed to get gilrs handle");

    info!("{} gamepad(s) found", gilrs.gamepads().count());
    for (_id, gamepad) in gilrs.gamepads() {
        info!("{} is {:?}", gamepad.name(), gamepad.power_info());
    }

    let mut message_data = InputMessage {
        gamepads: HashMap::new(),
        time: std::time::SystemTime::now().into(),
    };

    loop {
        while let Some(gilrs_event) = gilrs.next_event() {
            let gamepad_id: usize = gilrs_event.id.into();
            let gamepad_data = message_data.gamepads.entry(gamepad_id).or_default();

            gamepad_data.last_event_time = std::time::SystemTime::now().into();
            match gilrs_event.event {
                gilrs::EventType::ButtonPressed(button, _) => {
                    *gamepad_data
                        .button_down_event_counter
                        .entry(button.into())
                        .or_default() += 1;
                }
                gilrs::EventType::ButtonReleased(button, _) => {
                    *gamepad_data
                        .button_up_event_counter
                        .entry(button.into())
                        .or_default() += 1;
                }
                gilrs::EventType::AxisChanged(axis, value, _) => {
                    gamepad_data.axis_state.insert(axis.into(), value);
                }
                gilrs::EventType::Connected => {
                    gamepad_data.connected = true;
                    info!("Gamepad {} - {} connected", gamepad_id, gamepad_data.name)
                }
                gilrs::EventType::Disconnected => {
                    gamepad_data.connected = false;
                    warn!(
                        "Gamepad {} - {} disconnected",
                        gamepad_id, gamepad_data.name
                    )
                }
                _ => {}
            }
        }

        if let Some((gamepad_id, gamepad)) = gilrs.gamepads().next() {
            let gamepad_id: usize = gamepad_id.into();
            let gamepad_data = message_data.gamepads.entry(gamepad_id).or_default();

            gamepad_data.connected = gamepad.is_connected();
            gamepad_data.name = gamepad.name().to_string();

            if gamepad.is_connected() {
                for button in Button::all_gilrs_buttons() {
                    gamepad_data
                        .button_pressed
                        .insert(Button::from(*button), gamepad.is_pressed(*button));
                }

                // should we also get stick values here or use events?
                // let x = gamepad.value(gilrs::Axis::LeftStickY);
                // let x = if x.abs() > 0.2 { x } else { 0.0 };
            }
        }

        message_data.time = std::time::SystemTime::now().into();
        let json = serde_json::to_string(&message_data)?;
        gamepad_publisher
            .put(json)
            .res()
            .await
            .map_err(HopperRemoteError::ZenohError)?;
        tokio::time::sleep(Duration::from_millis(args.sleep_ms)).await;
    }
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct InputMessage {
    gamepads: HashMap<usize, GamepadMessage>,
    time: DateTime<Utc>,
}

#[derive(Debug, Deserialize, Serialize, Default, JsonSchema)]
pub struct GamepadMessage {
    name: String,
    button_down_event_counter: BTreeMap<Button, usize>,
    button_up_event_counter: BTreeMap<Button, usize>,
    button_pressed: BTreeMap<Button, bool>,
    axis_state: BTreeMap<Axis, f32>,
    connected: bool,
    last_event_time: DateTime<Utc>,
}

impl GamepadMessage {}

#[derive(
    Debug, Deserialize, Serialize, PartialEq, Eq, Hash, PartialOrd, Ord, Clone, Copy, JsonSchema,
)]
pub enum Button {
    South,
    East,
    North,
    West,
    C,
    Z,
    LeftTrigger,
    LeftTrigger2,
    RightTrigger,
    RightTrigger2,
    Select,
    Start,
    Mode,
    LeftThumb,
    RightThumb,
    DPadUp,
    DPadDown,
    DPadLeft,
    DPadRight,
    Unknown,
}

impl Button {
    pub fn all_gilrs_buttons() -> &'static [gilrs::ev::Button] {
        &[
            gilrs::ev::Button::South,
            gilrs::ev::Button::East,
            gilrs::ev::Button::North,
            gilrs::ev::Button::West,
            gilrs::ev::Button::C,
            gilrs::ev::Button::Z,
            gilrs::ev::Button::LeftTrigger,
            gilrs::ev::Button::LeftTrigger2,
            gilrs::ev::Button::RightTrigger,
            gilrs::ev::Button::RightTrigger2,
            gilrs::ev::Button::Select,
            gilrs::ev::Button::Start,
            gilrs::ev::Button::Mode,
            gilrs::ev::Button::LeftThumb,
            gilrs::ev::Button::RightThumb,
            gilrs::ev::Button::DPadUp,
            gilrs::ev::Button::DPadDown,
            gilrs::ev::Button::DPadLeft,
            gilrs::ev::Button::DPadRight,
        ]
    }
}

impl From<gilrs::ev::Button> for Button {
    fn from(value: gilrs::ev::Button) -> Self {
        match value {
            gilrs::ev::Button::South => Button::South,
            gilrs::ev::Button::East => Button::East,
            gilrs::ev::Button::North => Button::North,
            gilrs::ev::Button::West => Button::West,
            gilrs::ev::Button::C => Button::C,
            gilrs::ev::Button::Z => Button::Z,
            gilrs::ev::Button::LeftTrigger => Button::LeftTrigger,
            gilrs::ev::Button::LeftTrigger2 => Button::LeftTrigger2,
            gilrs::ev::Button::RightTrigger => Button::RightTrigger,
            gilrs::ev::Button::RightTrigger2 => Button::RightTrigger2,
            gilrs::ev::Button::Select => Button::Select,
            gilrs::ev::Button::Start => Button::Start,
            gilrs::ev::Button::Mode => Button::Mode,
            gilrs::ev::Button::LeftThumb => Button::LeftThumb,
            gilrs::ev::Button::RightThumb => Button::RightThumb,
            gilrs::ev::Button::DPadUp => Button::DPadUp,
            gilrs::ev::Button::DPadDown => Button::DPadDown,
            gilrs::ev::Button::DPadLeft => Button::DPadLeft,
            gilrs::ev::Button::DPadRight => Button::DPadRight,
            gilrs::ev::Button::Unknown => Button::Unknown,
        }
    }
}

#[derive(
    Debug, Deserialize, Serialize, PartialEq, Eq, Hash, PartialOrd, Ord, Clone, Copy, JsonSchema,
)]
pub enum Axis {
    LeftStickX,
    LeftStickY,
    LeftZ,
    RightStickX,
    RightStickY,
    RightZ,
    DPadX,
    DPadY,
    Unknown,
}

impl Axis {
    pub fn all_axes() -> &'static [Axis] {
        &[
            Axis::LeftStickX,
            Axis::LeftStickY,
            Axis::LeftZ,
            Axis::RightStickX,
            Axis::RightStickY,
            Axis::RightZ,
            Axis::DPadX,
            Axis::DPadY,
        ]
    }
}

impl From<gilrs::ev::Axis> for Axis {
    fn from(value: gilrs::ev::Axis) -> Self {
        match value {
            gilrs::ev::Axis::LeftStickX => Axis::LeftStickX,
            gilrs::ev::Axis::LeftStickY => Axis::LeftStickY,
            gilrs::ev::Axis::LeftZ => Axis::LeftZ,
            gilrs::ev::Axis::RightStickX => Axis::RightStickX,
            gilrs::ev::Axis::RightStickY => Axis::RightStickY,
            gilrs::ev::Axis::RightZ => Axis::RightZ,
            gilrs::ev::Axis::DPadX => Axis::DPadX,
            gilrs::ev::Axis::DPadY => Axis::DPadY,
            gilrs::ev::Axis::Unknown => Axis::Unknown,
        }
    }
}

pub fn setup_tracing(verbosity_level: u8) {
    let filter = match verbosity_level {
        0 => tracing::level_filters::LevelFilter::WARN,
        1 => tracing::level_filters::LevelFilter::INFO,
        2 => tracing::level_filters::LevelFilter::DEBUG,
        3 => tracing::level_filters::LevelFilter::TRACE,
        _ => tracing::level_filters::LevelFilter::TRACE,
    };

    tracing_subscriber::fmt().with_max_level(filter).init();
}

#[derive(Error, Debug)]
pub enum HopperRemoteError {
    #[error("Zenoh error {0:?}")]
    ZenohError(#[from] zenoh::Error),
}
