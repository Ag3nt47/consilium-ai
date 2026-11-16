#!/usr/bin/env node
import readline from "node:readline";
import qrcode from "qrcode";

const rl = readline.createInterface({
  input: process.stdin,
  output: process.stdout,
  terminal: false,
});

rl.on("line", async (line) => {
  const trimmed = line.trim();
  if (trimmed.startsWith("sgnl://") || trimmed.startsWith("tsdevice:/")) {
    console.log("\nScan this QR code with Signal on your phone (Settings > Linked Devices > Link New Device):\n");
    try {
      const qr = await qrcode.toString(trimmed, { type: "terminal", small: true });
      console.log(qr);
    } catch (e) {
      console.error("Failed to render QR code:", e);
    }
    console.log("Raw link:", trimmed);
    console.log("\nWaiting for device approval on your phone...\n");
  } else {
    console.log(line);
  }
});
