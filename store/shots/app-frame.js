// Loads the rdm app UI (with its preview backend) in the frame and runs
// `setup(win)` once the mock is ready.
export function loadApp(frame, setup) {
  frame.src = '../../crates/rdm-gui/ui/index.html';
  frame.addEventListener('load', () => {
    const win = frame.contentWindow;
    const wait = () => (win.rdmMock ? setTimeout(() => setup(win), 300) : setTimeout(wait, 50));
    wait();
  });
}
