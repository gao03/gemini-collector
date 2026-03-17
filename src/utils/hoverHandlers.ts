import React from "react";

/** Returns onMouseEnter/onMouseLeave handlers that toggle background color. */
export function hoverHandlers(hoverBg: string): {
  onMouseEnter: React.MouseEventHandler;
  onMouseLeave: React.MouseEventHandler;
} {
  return {
    onMouseEnter: (e) => ((e.currentTarget as HTMLElement).style.background = hoverBg),
    onMouseLeave: (e) => ((e.currentTarget as HTMLElement).style.background = "transparent"),
  };
}
