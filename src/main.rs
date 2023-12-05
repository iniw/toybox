#![no_std]
#![no_main]
#![feature(type_alias_impl_trait)]

use defmt::*;
use defmt_rtt as _;
use embassy_executor::{main, task, Spawner};
use embassy_futures::select::{select, Either::*};
use embassy_stm32::{exti::*, gpio::*};
use embassy_sync::{blocking_mutex::raw::ThreadModeRawMutex, signal::Signal};
use embassy_time::Timer;
use panic_probe as _;

static SIGNAL: Signal<ThreadModeRawMutex, bool> = Signal::new();
const BLINK_INITIALLY: bool = true;

#[task]
async fn button_task(p: AnyPin, c: AnyChannel) {
    let input = Input::new(p, Pull::None);
    let mut button = ExtiInput::new(input, c);
    let mut state = BLINK_INITIALLY;

    loop {
        button.wait_for_falling_edge().await;

        state = !state;
        SIGNAL.signal(state);
        debug!("button pressed");

        // disallow quick button presses since the signal is edge-triggered
        // so we might run into false-positives on bad hardware
        Timer::after_millis(100).await;
    }
}

#[task]
async fn led_task(p: AnyPin) {
    let mut led = Output::new(p, Level::High, Speed::Low);
    let mut should_blink = BLINK_INITIALLY;

    loop {
        match should_blink {
            true => match select(Timer::after_secs(1), SIGNAL.wait()).await {
                First(_) => {
                    led.toggle();
                    debug!("blink");
                }
                Second(signal) => {
                    should_blink = signal;
                    led.set_level(should_blink.into());
                    debug!("signal changed");
                }
            },
            false => {
                should_blink = SIGNAL.wait().await;
                led.set_level(should_blink.into());
                debug!("signal changed");
            }
        }
    }
}

#[main]
async fn main(spawner: Spawner) {
    info!("hi");

    let p = embassy_stm32::init(Default::default());
    unwrap!(spawner.spawn(button_task(p.PA0.degrade(), p.EXTI0.degrade())));
    unwrap!(spawner.spawn(led_task(p.PC6.degrade())));
}
