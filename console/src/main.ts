import { mount } from "svelte";
import "./styles/broadsheet.css";
import "./styles/fonts.css";
import "./styles/console.css";
import App from "./App.svelte";

const target = document.getElementById("app");
if (!target) {
  throw new Error("#app mount point not found");
}

const app = mount(App, { target });

export default app;
