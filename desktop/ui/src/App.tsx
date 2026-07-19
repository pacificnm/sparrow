import { useCallback, useEffect, useState } from "react";

import { Typography } from "@nest/components";

import { AnalystPanel } from "./components/AnalystPanel";
import { HostDetail } from "./components/HostDetail";
import { HostList } from "./components/HostList";
import { ProblemsPanel } from "./components/ProblemsPanel";
import { TitleBar } from "./components/TitleBar";
import {
  type Host,
  type MetricItem,
  type Problem,
  getActiveProblems,
  getHostItems,
  listHosts,
  runAnalysis,
} from "./lib/api";
import { applyThemeRootBlock, fetchThemeCss } from "./lib/nest";
import { quitApp } from "./lib/tauri";

export function App() {
  const [hosts, setHosts] = useState<Host[]>([]);
  const [selectedHostId, setSelectedHostId] = useState<string | null>(null);
  const [items, setItems] = useState<MetricItem[]>([]);
  const [problems, setProblems] = useState<Problem[]>([]);
  const [loadError, setLoadError] = useState<string | null>(null);

  useEffect(() => {
    void (async () => {
      try {
        const theme = await fetchThemeCss();
        applyThemeRootBlock(theme.root_block);
      } catch {
        // No running Tauri host (e.g. a plain browser dev preview) — the
        // default :root fallback in index.css still applies.
      }
    })();
  }, []);

  const refreshHosts = useCallback(async () => {
    try {
      setHosts(await listHosts());
      setLoadError(null);
    } catch (err) {
      setLoadError(err instanceof Error ? err.message : String(err));
    }
  }, []);

  const refreshProblems = useCallback(async () => {
    try {
      setProblems(await getActiveProblems());
    } catch (err) {
      setLoadError(err instanceof Error ? err.message : String(err));
    }
  }, []);

  useEffect(() => {
    void refreshHosts();
    void refreshProblems();
  }, [refreshHosts, refreshProblems]);

  useEffect(() => {
    if (!selectedHostId) {
      setItems([]);
      return;
    }
    void (async () => {
      try {
        setItems(await getHostItems(selectedHostId));
      } catch (err) {
        setLoadError(err instanceof Error ? err.message : String(err));
      }
    })();
  }, [selectedHostId]);

  return (
    <div className="flex h-screen min-h-0 flex-col bg-nest-background text-nest-foreground">
      <TitleBar
        title="Sparrow"
        onQuit={() => void quitApp()}
        onAbout={() => window.alert("Sparrow Desktop\nFleet health dashboard")}
      />

      <div className="mx-auto flex w-full max-w-5xl flex-1 flex-col gap-8 overflow-auto p-8">
        <header>
          <Typography variant="h4">Sparrow</Typography>
          <Typography variant="body2" className="text-nest-muted">
            Fleet health dashboard
          </Typography>
        </header>

        {loadError && (
          <Typography variant="body2" className="text-nest-error">
            {loadError}
          </Typography>
        )}

        <section>
          <Typography variant="h6" className="mb-3">
            Hosts
          </Typography>
          <HostList hosts={hosts} selectedHostId={selectedHostId} onSelectHost={setSelectedHostId} />
        </section>

        {selectedHostId && (
          <section>
            <HostDetail hostId={selectedHostId} items={items} />
          </section>
        )}

        <section>
          <Typography variant="h6" className="mb-3">
            Problems
          </Typography>
          <ProblemsPanel problems={problems} />
        </section>

        <section>
          <Typography variant="h6" className="mb-3">
            AI Health Analyst
          </Typography>
          <AnalystPanel problems={problems} onRunAnalysis={runAnalysis} />
        </section>
      </div>
    </div>
  );
}
