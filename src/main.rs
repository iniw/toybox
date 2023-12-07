#![no_std]
#![no_main]
#![feature(type_alias_impl_trait)]

use core::cmp::Ordering;

use defmt::*;
use defmt::{todo, unreachable};
use defmt_rtt as _;
use embassy_executor::{main, task, Spawner};
use embassy_futures::select::{select, Either::*};
use embassy_stm32::{exti, gpio::*};
use embassy_sync::channel::*;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Instant, Timer};
use heapless::Vec;
use panic_probe as _;
use static_cell::make_static;

use crate::leds::LedSignal;
use crate::recipes::{RecipeControllerSignal, RecipeExecutorChannel};
use crate::stations::{PerStationData, StationStatusSignal, MAX_NUM_STATIONS};

mod buttons;
mod leds;
mod recipes;
mod stations;

#[main]
async fn main(spawner: Spawner) {
    info!("hi");

    let p = embassy_stm32::init(Default::default());

    let silly_array: PerStationData<&'static RecipeControllerSignal> =
        [make_static!(RecipeControllerSignal::new()); MAX_NUM_STATIONS];
    let button_peripherals = [(p.PA0.degrade(), exti::Channel::degrade(p.EXTI0))];

    let recipe_executor_channel = make_static!(RecipeExecutorChannel::new());
    let recipe_controller_signals: &'static PerStationData<&'static RecipeControllerSignal> =
        make_static!(silly_array);

    let silly_array: PerStationData<&'static _> =
        [make_static!(StationStatusSignal::new()); MAX_NUM_STATIONS];
    let station_status_signals: &'static PerStationData<&'static StationStatusSignal> =
        make_static!(silly_array);

    let led_signals: PerStationData<&'static LedSignal> =
        [make_static!(Signal::new()); MAX_NUM_STATIONS];
    let led_peripherals = [(p.PC6.degrade())];

    buttons::spawn_tasks(&spawner, button_peripherals, recipe_controller_signals);
    leds::spawn_tasks(&spawner, led_peripherals, &led_signals);
    recipes::spawn_tasks(
        &spawner,
        recipe_executor_channel,
        &recipe_controller_signals,
        &station_status_signals,
    );
    stations::spawn_tasks(&spawner, station_status_signals, &led_signals);
}
