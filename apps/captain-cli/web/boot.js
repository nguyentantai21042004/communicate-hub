"use strict";
(function () {
  if (!window.Captain || typeof Captain.start !== "function") {
    const main = document.getElementById("main");
    if (main) main.textContent = "Captain UI failed to start.";
    return;
  }
  Captain.start();
})();
