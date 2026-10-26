// Checks the timeline page's slider: a mouse drag (QAROS Ultralight panels) and a touch
// drag (QAROS CEF panels) on the track send the viewer seeks, thinned to one per interval with
// the last position always sent, and Play sends a play request. Reads the rendered page from
// stdin; `page.rs` runs it under `cargo test`.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

import { TRACK_LEFT, TRACK_WIDTH, runPage } from "./page_stand_ins.mjs";

const { clock, document, elements, images, timers } = runPage(readFileSync(0, "utf8"));
const track = elements.track;
const thumb = elements.thumb;

function atFraction(fraction) {
    return TRACK_LEFT + fraction * TRACK_WIDTH;
}

// The page also polls the viewer's position on a timer of its own, which these checks leave be.
const POSITION_INTERVAL = 250;

function seekTimers() {
    return timers.filter((timer) => timer.delay !== POSITION_INTERVAL);
}

function runSeekTimer() {
    const index = timers.findIndex((timer) => timer.delay !== POSITION_INTERVAL);
    const [timer] = timers.splice(index, 1);
    timer.callback();
    return timer.delay;
}

let requestsSeen = 0;
function newTimelineRequests() {
    const all = images
        .map((image) => image.src)
        .filter((source) => source.startsWith("/timeline?"))
        .map((source) => source.replace(/&n=\d+-t\d+$/, ""));
    const fresh = all.slice(requestsSeen);
    requestsSeen = all.length;
    return fresh;
}

function mouse(clientX) {
    return { clientX, preventDefault() {} };
}

function touch(clientX) {
    const event = {
        changedTouches: [{ clientX }],
        prevented: false,
        preventDefault() {
            event.prevented = true;
        },
    };
    return event;
}

assert.equal(seekTimers().length, 0);

track.dispatch("mousedown", mouse(atFraction(0.25)));
assert.deepEqual(newTimelineRequests(), ["/timeline?at=0.2500"], "a press seeks at once");
assert.equal(thumb.style.left, "25.00%");

document.dispatch("mousemove", mouse(atFraction(0.5)));
document.dispatch("mousemove", mouse(atFraction(0.75)));
assert.deepEqual(newTimelineRequests(), [], "moves within the interval wait");
assert.equal(seekTimers().length, 1, "one deferred seek for the waiting moves");
assert.equal(thumb.style.left, "75.00%", "the thumb follows every move");
clock.now += 80;
assert.equal(runSeekTimer(), 80);
assert.deepEqual(newTimelineRequests(), ["/timeline?at=0.7500"], "the latest waiting move goes");

document.dispatch("mouseup", mouse(atFraction(1.5)));
assert.deepEqual(newTimelineRequests(), ["/timeline?at=1.0000"], "a release past the end ends it");
document.dispatch("mousemove", mouse(atFraction(0.1)));
assert.deepEqual(newTimelineRequests(), [], "moves after the release are not seeks");

elements.play.dispatch("click", mouse(0));
assert.deepEqual(newTimelineRequests(), ["/timeline?play"]);
assert.equal(thumb.style.left, "100.00%", "the thumb stays where it was until a position comes");

clock.now += 1000;
const touchStart = touch(atFraction(0));
track.dispatch("touchstart", touchStart);
assert.ok(touchStart.prevented, "the touch makes no mouse events");
const touchMove = touch(atFraction(0.3));
clock.now += 100;
document.dispatch("touchmove", touchMove);
assert.ok(touchMove.prevented, "a drag on the slider does not pan the page");
document.dispatch("touchend", touch(atFraction(0.5)));
assert.deepEqual(newTimelineRequests(), [
    "/timeline?at=0.0000",
    "/timeline?at=0.3000",
    "/timeline?at=0.5000",
]);

const strayMove = touch(atFraction(0.9));
document.dispatch("touchmove", strayMove);
assert.ok(!strayMove.prevented, "a touch elsewhere is left to the page");
assert.deepEqual(newTimelineRequests(), []);

console.log("page scrub: ok");
