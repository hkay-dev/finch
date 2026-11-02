// Prints the display ids whose current Space is a full-screen app's (Space type 4), one per line,
// and nothing when none is. bg-launch.sh and editor-bench.sh refuse to start a test window while one
// shows: an accessory app's window ordered in on a full-screen Space lands in front of that app
// (finch#jxb8). Run: osascript -l JavaScript scripts/full-screen-space.js
ObjC.import('CoreGraphics');
ObjC.bindFunction('CGSMainConnectionID', ['int', []]);
ObjC.bindFunction('CGSCopyManagedDisplaySpaces', ['id', ['int']]);
const displays = ObjC.deepUnwrap($.CGSCopyManagedDisplaySpaces($.CGSMainConnectionID())) || [];
displays
    .filter((display) => display['Current Space'] && display['Current Space'].type === 4)
    .map((display) => display['Display Identifier'])
    .join('\n');
