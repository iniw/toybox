#![no_std]
#![no_main]
#![feature(type_alias_impl_trait)]
#![feature(trace_macros)]

use defmt_rtt as _;
use embassy_stm32::exti::*;
use embassy_stm32::gpio::Pin;
use panic_probe as _;

use crate::leds::LedSignal;
use crate::recipes::RecipeControllerSignal;
use crate::stations::StationStatusSignal;

mod buttons;
mod leds;
mod recipes;
mod stations;

#[embassy_executor::main]
async fn main(spawner: embassy_executor::Spawner) {
    let p = embassy_stm32::init(Default::default());

    let recipe_controller_signals = stations::make_per_station_static_data!(RecipeControllerSignal);
    let station_status_signals = stations::make_per_station_static_data!(StationStatusSignal);
    let led_signals = stations::make_per_station_static_data!(LedSignal);

    let buttons_peripherals = [buttons::Peripherals {
        button_pin: p.PA0.degrade(),
        exti_channel: p.EXTI0.degrade(),
    }];

    let leds_peripherals = [leds::Peripherals {
        led_pin: p.PC6.degrade(),
    }];

    recipes::spawn_tasks(&spawner, station_status_signals, recipe_controller_signals);
    stations::spawn_tasks(&spawner, station_status_signals, led_signals);
    leds::spawn_tasks(&spawner, leds_peripherals, led_signals);
    buttons::spawn_tasks(&spawner, buttons_peripherals, recipe_controller_signals);
}
