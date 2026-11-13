import { t, locale } from "./i18n";
import {
  accountLabel,
  type AccountInfo,
  type Chain,
  type EndpointInfo,
} from "./api";
import { viewHash, type View } from "./navigation";
import { useEffect } from "react";
import { open, syncDevices, type Ops } from "./ops";
import { Attention, endpointIssue } from "./Attention";
import {
  Cable,
  connectionCopy,
  fixAction,
  fixedOnSide,
  presenceText,
  routeState,
  routeTone,
} from "./route";

export const joinNames = (names: string[]) =>
  new Intl.ListFormat(locale(), { type: "conjunction" }).format(names);
export const computerName = (info?: EndpointInfo) =>
  info?.target
    ? (info.target_name ?? t("Unnamed computer"))
    : t("No computer paired");
// The last presence the backend recorded; a running connection reports a fresher one.
export const storedPresence = (info: EndpointInfo) =>
  info.target_online == null
    ? null
    : info.target_online
      ? t("Online")
      : t("Offline");
export const deviceName = (info?: EndpointInfo) =>
  info?.paired_device
    ? (info.paired_device.name ?? t("Unnamed device"))
    : t("No device paired");

// The end that needs attention carries the mark; the cable only shows the connection.
const cardClass = (
  info: EndpointInfo,
  accounts: AccountInfo[],
  chain?: Chain,
) =>
  endpointIssue(info, accounts, chain)
    ? "jack-card needs-attention"
    : "jack-card";

function Title({
  view,
  ops,
  children,
}: {
  view: View;
  ops: Ops;
  children: string;
}) {
  return (
    <h3 className="jack-title">
      <a
        href={`#${viewHash(view)}`}
        onClick={(event) => {
          event.preventDefault();
          ops.setNotice("");
          ops.go(view);
        }}
      >
        {children}
      </a>
    </h3>
  );
}

function ControllerCard({
  info,
  accounts,
  chain,
  ops,
  children,
}: {
  info: EndpointInfo;
  accounts: AccountInfo[];
  chain?: Chain;
  ops: Ops;
  children?: React.ReactNode;
}) {
  return (
    <article
      className={cardClass(info, accounts, chain)}
      aria-label={t("Controller {0}", [info.endpoint.display_name])}
    >
      <Title
        view={{ page: "home", controller: info.endpoint.device_id }}
        ops={ops}
      >
        {info.endpoint.display_name}
      </Title>
      <p className="jack-meta">{deviceName(info)}</p>
      <p className="jack-account">
        {accountLabel(info.endpoint.account, accounts)}
      </p>
      <Attention info={info} accounts={accounts} chain={chain} ops={ops} />
      {children}
    </article>
  );
}

function DeviceCard({
  info,
  presence,
  accounts,
  chain,
  ops,
  children,
}: {
  info: EndpointInfo;
  presence?: string | null;
  accounts: AccountInfo[];
  chain?: Chain;
  ops: Ops;
  children?: React.ReactNode;
}) {
  return (
    <article
      className={cardClass(info, accounts, chain)}
      aria-label={t("Controlled device {0}", [info.endpoint.display_name])}
    >
      <Title view={{ page: "home", device: info.endpoint.device_id }} ops={ops}>
        {info.endpoint.display_name}
      </Title>
      <p className="jack-meta">
        {computerName(info)}
        {(presence ?? storedPresence(info)) && (
          <span className="jack-presence">
            {presence ?? storedPresence(info)}
          </span>
        )}
      </p>
      <p className="jack-account">
        {accountLabel(info.endpoint.account, accounts)}
      </p>
      <Attention info={info} accounts={accounts} chain={chain} ops={ops} />
      {children}
    </article>
  );
}

export function StartStop({
  chain,
  ops,
  compact = false,
  here,
}: {
  chain: Chain;
  ops: Ops;
  compact?: boolean;
  here?: "controller" | "device";
}) {
  const input = chain.binding.in_id;
  const state = routeState(chain);
  // On the overview the fix is on the marked card; a detail page links to the other end.
  const fix = compact || here === chain.side ? null : fixAction(chain);
  return (
    <div className="actions">
      {chain.status === "failed" && !fixedOnSide(state) && (
        <button
          className="primary"
          onClick={() =>
            void ops.perform(t("Retry connection"), async () => {
              await ops.api(`/inputs/${input}/start`, "POST");
            })
          }
        >
          {t("Retry")}
        </button>
      )}
      {chain.status === "failed" && fix && (
        <button
          onClick={() => {
            ops.setNotice("");
            ops.go(fix[1]);
          }}
        >
          {fix[0]}
        </button>
      )}
      {(chain.status !== "failed" ||
        (chain.enabled && (!compact || fixedOnSide(state)))) && (
        <button
          className={chain.enabled ? "" : "primary"}
          onClick={() =>
            void ops.perform(
              chain.enabled ? t("Stop") : t("Start"),
              async () => {
                await ops.api(
                  `/inputs/${input}/${chain.enabled ? "stop" : "start"}`,
                  "POST",
                );
              },
            )
          }
        >
          {chain.enabled ? t("Stop") : t("Start")}
        </button>
      )}
    </div>
  );
}

export function Home({
  accounts,
  endpoints,
  chains,
  ops,
}: {
  accounts: AccountInfo[];
  endpoints: EndpointInfo[];
  chains: Chain[];
  ops: Ops;
}) {
  const controllers = endpoints.filter((i) => i.endpoint.kind === "in");
  // Opening the overview checks every device and computer, like Desktop's lists.
  useEffect(() => syncDevices(ops.api, endpoints), []);
  const devices = endpoints.filter((i) => i.endpoint.kind === "out");
  const find = (id: string) =>
    endpoints.find((item) => item.endpoint.device_id === id);
  const boundIn = new Set(chains.map((c) => c.binding.in_id));
  const boundOut = new Set(chains.map((c) => c.binding.out_id));
  const freeControllers = controllers.filter(
    (i) => !boundIn.has(i.endpoint.device_id),
  );
  const freeDevices = devices.filter(
    (i) => !boundOut.has(i.endpoint.device_id),
  );
  const create = (role: "in" | "out") => open(ops, { kind: "create", role });
  const ready = (info: EndpointInfo) =>
    info.lifecycle === "active" && !endpointIssue(info, accounts);

  return (
    <div className="bay">
      <div className="bay-row bay-head">
        <div className="bay-col-head">
          <h2>{t("Controllers")}</h2>
          <button onClick={() => create("in")}>{t("New controller")}</button>
        </div>
        <span aria-hidden="true" />
        <div className="bay-col-head">
          <h2>{t("Controlled devices")}</h2>
          <button onClick={() => create("out")}>
            {t("New controlled device")}
          </button>
        </div>
      </div>

      {!endpoints.length && (
        <div className="bay-row bay-empty">
          <button className="jack-slot" onClick={() => create("in")}>
            <strong>{t("Add a controller")}</strong>
            <span>
              {t(
                "The device you control from: a phone, tablet or computer with ChatGPT. The controlled computer appears there as a remote device.",
              )}
            </span>
          </button>
          <Cable tone="idle" />
          <button className="jack-slot" onClick={() => create("out")}>
            <strong>{t("Add a controlled device")}</strong>
            <span>
              {t(
                "The computer running Codex you want to control. Its ChatGPT account can differ from the controller's.",
              )}
            </span>
          </button>
        </div>
      )}

      {chains.map((chain) => {
        const controller = find(chain.binding.in_id);
        const device = find(chain.binding.out_id);
        const state = routeState(chain);
        const tone = routeTone(state);
        const [label, help] = connectionCopy(state);
        return (
          <section
            className={`bay-row bay-pair tone-${tone}`}
            key={chain.binding.in_id}
            aria-label={t("Connection {0}", [
              controller?.endpoint.display_name,
            ])}
          >
            {controller ? (
              <ControllerCard
                info={controller}
                accounts={accounts}
                chain={chain}
                ops={ops}
              />
            ) : (
              <span />
            )}
            <div className="bay-link">
              <strong className="bay-state" role="status" title={help}>
                {label}
              </strong>
              <Cable tone={tone} />
              <StartStop chain={chain} ops={ops} compact />
            </div>
            {device ? (
              <DeviceCard
                info={device}
                presence={presenceText(state)}
                accounts={accounts}
                chain={chain}
                ops={ops}
              />
            ) : (
              <span />
            )}
          </section>
        );
      })}

      {(freeControllers.length > 0 || freeDevices.length > 0) &&
        endpoints.length > 0 && (
          <section className="bay-free" aria-labelledby="bay-free-title">
            <h2 id="bay-free-title">{t("Not connected")}</h2>
            <div className="bay-row bay-free-row">
              <div className="bay-stack">
                <h3 className="bay-stack-head">{t("Controllers")}</h3>
                {freeControllers.map((info) => (
                  <ControllerCard
                    key={info.endpoint.device_id}
                    info={info}
                    accounts={accounts}
                    ops={ops}
                  >
                    {ready(info) && (
                      <div className="actions">
                        <button
                          onClick={() =>
                            open(ops, {
                              kind: "connect",
                              input: info.endpoint.device_id,
                            })
                          }
                        >
                          {t("Connect to a device")}
                        </button>
                      </div>
                    )}
                  </ControllerCard>
                ))}
                {!freeControllers.length && (
                  <p className="bay-none">
                    {t("Every controller is connected.")}
                  </p>
                )}
              </div>
              <span aria-hidden="true" />
              <div className="bay-stack">
                <h3 className="bay-stack-head">{t("Controlled devices")}</h3>
                {freeDevices.map((info) => (
                  <DeviceCard
                    key={info.endpoint.device_id}
                    info={info}
                    accounts={accounts}
                    ops={ops}
                  >
                    {ready(info) && (
                      <div className="actions">
                        <button
                          onClick={() =>
                            open(ops, {
                              kind: "connect",
                              output: info.endpoint.device_id,
                            })
                          }
                        >
                          {t("Connect a controller")}
                        </button>
                      </div>
                    )}
                  </DeviceCard>
                ))}
                {!freeDevices.length && (
                  <p className="bay-none">
                    {t("Every controlled device is in use.")}
                  </p>
                )}
              </div>
            </div>
          </section>
        )}
    </div>
  );
}
