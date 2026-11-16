import { getCurrentWindow } from "@tauri-apps/api/window";
import Popup from "./popup/Popup";
import SettingsWindow from "./settings/SettingsWindow";

// Both windows load this page; the window label picks the view.
export default function App() {
  const label = getCurrentWindow().label;
  return label === "popup" ? <Popup /> : <SettingsWindow />;
}
