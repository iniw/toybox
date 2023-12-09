use defmt::{unreachable, *};
use embassy_executor::*;
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, signal};

use crate::leds;

macro_rules! make_per_station_data {
    ($e:expr) => {{
        [$e; crate::stations::MAX_STATIONS]
    }};
}

macro_rules! make_per_station_static_data {
    ($e:expr) => {{
        [static_cell::make_static!($e); crate::stations::MAX_STATIONS]
    }};
}

pub(crate) use make_per_station_data;
pub(crate) use make_per_station_static_data;

struct Context {
    station: Station,
    station_status_signal: &'static Signal,
    led_signal: &'static leds::Signal,
}

#[derive(Format, Clone, Copy)]
pub enum Status {
    Free,
    WaitingToScald,
    Scalding,
    WaitingToAttack,
    Attacking,
    Finalizing,
    Finished,
}

impl Status {
    pub fn advance(&mut self) {
        *self = match self {
            Status::WaitingToScald => Status::Scalding,
            Status::Scalding => Status::WaitingToAttack,
            Status::WaitingToAttack => Status::Attacking,
            Status::Attacking => Status::Finalizing,
            Status::Finalizing => Status::Finished,
            _ => unreachable!("cannot advance finished recipe"),
        }
    }
}

#[derive(Format, Clone, Copy, Eq, PartialEq, Ord, PartialOrd)]
pub struct Station(pub usize);

pub type PerStationData<T> = [T; MAX_STATIONS];
pub type PerStationStaticData<T> = PerStationData<&'static T>;

pub type Signal = signal::Signal<NoopRawMutex, Status>;

pub const MAX_STATIONS: usize = 1;

#[task(pool_size = MAX_STATIONS)]
async fn main_task(ctx: Context) {
    loop {
        let new_status = ctx.station_status_signal.wait().await;
        debug!("#{}: status changed [new = {}]", ctx.station, new_status);
        match new_status {
            Status::Free => {
                ctx.led_signal.signal(leds::Status::Off);
            }
            Status::WaitingToAttack | Status::WaitingToScald | Status::Finished => {
                ctx.led_signal.signal(leds::Status::Blinking);
            }
            _ => {
                ctx.led_signal.signal(leds::Status::On);
            }
        }
    }
}

pub(super) fn spawn_tasks(spawner: &Spawner, ctx: &'static crate::SharedContext) {
    for i in 0..MAX_STATIONS {
        unwrap!(spawner.spawn(main_task(Context {
            station: Station(i),
            station_status_signal: ctx.station_status_signals[i],
            led_signal: ctx.led_signals[i],
        })));
    }
}
