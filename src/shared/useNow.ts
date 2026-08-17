import { useEffect, useState } from "react";

export function useNow(interval = 1000): number {
  const [now, setNow] = useState(Date.now());
  useEffect(() => {
    const timer = window.setInterval(() => {
      setNow(Date.now());
    }, interval);
    return () => {
      window.clearInterval(timer);
    };
  }, [interval]);
  return now;
}
