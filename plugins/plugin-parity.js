import * as std from 'std';
import * as os from 'os';
globalThis.std = std;
globalThis.os = os;

function noop() {}

function getScrollLineHeight() {
    const el = document.createElement('div');
    el.style.fontSize = 'initial';
    el.style.display = 'none';
    document.body.appendChild(el);
    const fontSize = window.getComputedStyle(el).fontSize;
    document.body.removeChild(el);
    return fontSize ? window.parseInt(fontSize) : 16;
}
// To handle getScrollLineHeight
var SUMMARY = {};
globalThis.document = {
    createElement: () => ({ 'style': {} }),
    body: {
        appendChild: noop,
        removeChild: noop,
    },
    getElementById: (id) => {
        if (id !== 'summary') { throw 'oh no'; }
        return SUMMARY;
    },
};
globalThis.window = {
    getComputedStyle: () => 0,
    parseInt: noop,
};

std.loadScript("/work/bs-parity-main.js");
// Post-load overrides
var TOTAL_OUTPUT = [];
outputUI = function (note, parity, message, messageType, persistent = false) {
    TOTAL_OUTPUT.push([note, parity, message, messageType]);
};
clearOutput = noop;

// Prep globals
let parsed = JSON.parse(std.loadFile("/data/map.dat"));
notesArray = getNotes(parsed);
wallsArray = getWalls(parsed);
ready = true;

// Do the calc
checkParity();

// Return
console.log(JSON.stringify({summary: SUMMARY, detail: TOTAL_OUTPUT}));
