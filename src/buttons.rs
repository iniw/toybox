use defmt::debug;
use defmt::unwrap;
use embassy_executor::task;
use embassy_executor::Spawner;
use embassy_futures::select::{select, Either::*};
use embassy_stm32::exti;
use embassy_stm32::gpio::{AnyPin, Input, Pull};
use embassy_time::Timer;

use crate::recipes::*;
use crate::stations::*;

#[task(pool_size = MAX_NUM_STATIONS)]
async fn button_task(ctx: ButtonControlContext) {
    let (pin, exti_channel) = ctx.peripherals;

    let input = Input::new(pin, Pull::Up);
    let mut button = exti::ExtiInput::new(input, exti_channel);

    loop {
        button.wait_for_rising_edge().await;
        debug!("button pressed");

        match select(button.wait_for_falling_edge(), Timer::after_secs(3)).await {
            First(_) => {
                debug!("button released");
                ctx.recipe_controller_signal
                    .signal(RecipeControllerEvent::ButtonPress);
            }
            Second(_) => {
                debug!("button was held");
                ctx.recipe_controller_signal
                    .signal(RecipeControllerEvent::CancelRecipe);
            }
        }

        // disallow quick button presses since the signal is edge-triggered
        // so we might run into false-positives on bad hardware
        Timer::after_millis(100).await;
    }
}

type Peripherals = (AnyPin, exti::AnyChannel);

struct ButtonControlContext {
    peripherals: Peripherals,
    recipe_controller_signal: &'static RecipeControllerSignal,
}

pub fn spawn_tasks(
    spawner: &Spawner,
    peripherals: PerStationData<(AnyPin, exti::AnyChannel)>,
    recipe_controller_signals: &PerStationData<&'static RecipeControllerSignal>,
) {
    let mut iter = peripherals.into_iter();
    for i in 0..MAX_NUM_STATIONS {
        unwrap!(spawner.spawn(button_task(ButtonControlContext {
            peripherals: unwrap!(iter.next()),
            recipe_controller_signal: recipe_controller_signals[i]
        })));
    }
}
