use defmt::{debug, unreachable, unwrap, Format};
use embassy_executor::{task, Spawner};
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, signal::Signal};

use crate::leds::{LedSignal, LedStatus};

pub const MAX_NUM_STATIONS: usize = 1;

pub type PerStationData<T> = [T; MAX_NUM_STATIONS];
pub type PerStationStaticData<T> = PerStationData<&'static T>;

macro_rules! make_per_station_data {
    ($e:expr) => {{
        [$e; crate::stations::MAX_NUM_STATIONS]
    }};
}

macro_rules! make_per_station_static_data {
    ($e:expr) => {{
        use static_cell::make_static;
        [make_static!($e); crate::stations::MAX_NUM_STATIONS]
    }};
}

pub(crate) use make_per_station_data;
pub(crate) use make_per_station_static_data;

#[derive(Format, Clone, Copy)]
pub enum StationStatus {
    Free,
    WaitingToScald,
    Scalding,
    WaitingToAttack,
    Attacking,
    Finalizing,
    Finished,
}

impl StationStatus {
    pub fn advance(&mut self) {
        *self = match self {
            StationStatus::WaitingToScald => StationStatus::Scalding,
            StationStatus::Scalding => StationStatus::WaitingToAttack,
            StationStatus::WaitingToAttack => StationStatus::Attacking,
            StationStatus::Attacking => StationStatus::Finalizing,
            StationStatus::Finalizing => StationStatus::Finished,
            _ => unreachable!("cannot advance finished recipe"),
        }
    }
}

#[derive(Format, Clone, Copy, Eq, PartialEq, Ord, PartialOrd)]
pub struct Station(pub usize);

#[task(pool_size = MAX_NUM_STATIONS)]
async fn station_status_task(ctx: StationControlContext) {
    loop {
        let new_status = ctx.station_status_signal.wait().await;
        debug!("station #{} new status = {}", ctx.station, new_status);
        match new_status {
            StationStatus::Free => {
                ctx.led_signal.signal(LedStatus::Off);
            }
            StationStatus::WaitingToAttack
            | StationStatus::WaitingToScald
            | StationStatus::Finished => {
                ctx.led_signal.signal(LedStatus::Blinking);
            }
            _ => {
                ctx.led_signal.signal(LedStatus::On);
            }
        }
    }
}

pub type StationStatusSignal = Signal<NoopRawMutex, StationStatus>;

struct StationControlContext {
    station: Station,
    station_status_signal: &'static StationStatusSignal,
    led_signal: &'static LedSignal,
}

pub fn spawn_tasks(spawner: &Spawner, ctx: &'static crate::GlobalContext) {
    for i in 0..MAX_NUM_STATIONS {
        unwrap!(spawner.spawn(station_status_task(StationControlContext {
            station: Station(i),
            station_status_signal: ctx.station_status_signals[i],
            led_signal: ctx.led_signals[i],
        })));
    }
}
