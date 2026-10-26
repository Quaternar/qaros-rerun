// Runs the panel page's script against stand-ins for the browser APIs it uses: images, timers,
// the clock and the elements with their event listeners. Shared by the page checks that
// `page.rs` runs under `cargo test`.

import vm from "node:vm";

export const TRACK_LEFT = 40;
export const TRACK_WIDTH = 400;

class StandInElement {
    constructor() {
        this.src = "";
        this.textContent = "";
        this.style = { visibility: "hidden", left: "" };
        this.listeners = {};
    }

    addEventListener(type, listener) {
        this.listeners[type] = listener;
    }

    getBoundingClientRect() {
        return { left: TRACK_LEFT, width: TRACK_WIDTH };
    }

    dispatch(type, event) {
        this.listeners[type](event);
    }
}

export function runPage(page) {
    const script = page.match(/<script>([\s\S]*)<\/script>/)[1];
    const timers = [];
    const images = [];
    const clock = { now: 1760000000000 };
    const elements = {
        frame: new StandInElement(),
        status: new StandInElement(),
        track: new StandInElement(),
        thumb: new StandInElement(),
        play: new StandInElement(),
    };
    elements.status.textContent = "waiting for frames";
    const document = new StandInElement();
    document.getElementById = (id) => elements[id];

    class StandInImage {
        constructor() {
            this.src = "";
            images.push(this);
        }
    }

    class StandInDate {
        getTime() {
            return clock.now;
        }
    }

    vm.runInNewContext(script, {
        Image: StandInImage,
        Date: StandInDate,
        String,
        Math,
        setTimeout: (callback, delay) => timers.push({ callback, delay }),
        document,
    });

    function runNextTimer() {
        const timer = timers.shift();
        timer.callback();
        return timer.delay;
    }

    return { clock, document, elements, images, runNextTimer, timers };
}
