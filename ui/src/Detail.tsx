import { t, locale } from "./i18n";
import {
  accountLabel,
  type AccountInfo,
  type Chain,
  type EndpointInfo,
} from "./api";
import { useEffect } from "react";
import { open, pairController, syncDevices, type Ops } from "./ops";
import { viewHash, type View } from "./navigation";
import { Attention } from "./Attention";
import { computerName, deviceName, StartStop, storedPresence } from "./Home";
import {
  Cable,
  connectionCopy,
  presenceText,
  routeState,
  routeTone,
} from "./route";

function Jump({
  view,
  ops,
  children,
}: {
  view: View;
  ops: Ops;
  children: string;
}) {
  return (
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
  );
}

// The connection seen from one end: who is on the other side and how it's doing.
function LinkPanel({
  info,
  chain,
  endpoints,
  ops,
}: {
  info: EndpointInfo;
  chain?: Chain;
  endpoints: EndpointInfo[];
  ops: Ops;
}) {
  const controllerSide = info.endpoint.kind === "in";
  const ready =
    info.lifecycle === "active" &&
    info.registered &&
    (controllerSide || (!!info.target && !info.pairing_pending));
  if (!chain)
    return (
      <section className="link-panel tone-idle" aria-label={t("Connection")}>
        <div className="link-head">
          <div className="link-state">
            <strong>{t("Not connected")}</strong>
            <p>
              {controllerSide
                ? t("Connect it to a controlled device to start controlling.")
                : t("Connect a controller to start controlling this computer.")}
            </p>
          </div>
          {ready && (
            <button
              className="primary"
              onClick={() =>
                open(
                  ops,
                  controllerSide
                    ? { kind: "connect", input: info.endpoint.device_id }
                    : { kind: "connect", output: info.endpoint.device_id },
                )
              }
            >
              {controllerSide
                ? t("Connect to a device")
                : t("Connect a controller")}
            </button>
          )}
        </div>
      </section>
    );
  const find = (id: string) =>
    endpoints.find((item) => item.endpoint.device_id === id);
  const controller = find(chain.binding.in_id);
  const device = find(chain.binding.out_id);
  const state = routeState(chain);
  const tone = routeTone(state);
  const [label, help] = connectionCopy(state);
  return (
    <section className={`link-panel tone-${tone}`} aria-label={t("Connection")}>
      <div className="link-head">
        <div className="link-state" role="status">
          <strong>{label}</strong>
          <p>{help}</p>
        </div>
        <StartStop
          chain={chain}
          ops={ops}
          here={controllerSide ? "controller" : "device"}
        />
      </div>
      <div className="link-line">
        <span className={controllerSide ? "link-self" : ""}>
          {controllerSide || !controller ? (
            (controller?.endpoint.display_name ?? t("Unknown controller"))
          ) : (
            <Jump
              view={{ page: "home", controller: chain.binding.in_id }}
              ops={ops}
            >
              {controller.endpoint.display_name}
            </Jump>
          )}
        </span>
        <Cable tone={tone} />
        <span className={controllerSide ? "" : "link-self"}>
          {!controllerSide || !device ? (
            (device?.endpoint.display_name ?? t("Unknown controlled device"))
          ) : (
            <Jump
              view={{ page: "home", device: chain.binding.out_id }}
              ops={ops}
            >
              {device.endpoint.display_name}
            </Jump>
          )}
        </span>
      </div>
      <div className="actions link-change">
        {controllerSide && (
          <button
            onClick={() =>
              open(ops, { kind: "switch", device: chain.binding.in_id })
            }
          >
            {t("Switch device")}
          </button>
        )}
        <button
          className="danger-text"
          onClick={() =>
            open(ops, { kind: "unbind", device: chain.binding.in_id })
          }
        >
          {t("Disconnect")}
        </button>
      </div>
    </section>
  );
}

function Identity({
  info,
  accounts,
}: {
  info: EndpointInfo;
  accounts: AccountInfo[];
}) {
  return (
    <dl className="facts">
      <div>
        <dt>{t("Account")}</dt>
        <dd className="id-text">
          {accountLabel(info.endpoint.account, accounts)}
        </dd>
      </div>
      <div>
        <dt>{t("Fixed name")}</dt>
        <dd>{info.endpoint.identity_name}</dd>
      </div>
      <div>
        <dt>
          {info.endpoint.kind === "in" ? t("Input IN ID") : t("Output OUT ID")}
        </dt>
        <dd className="id-text fact-quiet">{info.endpoint.device_id}</dd>
      </div>
    </dl>
  );
}

function Missing({ ops, controller }: { ops: Ops; controller: boolean }) {
  return (
    <div className="empty">
      <h2>
        {controller
          ? t("This controller no longer exists")
          : t("This controlled device no longer exists")}
      </h2>
      <p>{t("It was deleted. Return to the overview to see what's left.")}</p>
      <button className="primary" onClick={() => ops.go({ page: "home" })}>
        {t("Back to overview")}
      </button>
    </div>
  );
}

export function ControllerDetail({
  id,
  accounts,
  endpoints,
  chains,
  ops,
}: {
  id: string;
  accounts: AccountInfo[];
  endpoints: EndpointInfo[];
  chains: Chain[];
  ops: Ops;
}) {
  const info = endpoints.find(
    (item) => item.endpoint.device_id === id && item.endpoint.kind === "in",
  );
  useEffect(() => {
    if (info) syncDevices(ops.api, [info]);
  }, [id]);
  if (!info) return <Missing ops={ops} controller />;
  const chain = chains.find((c) => c.binding.in_id === id);
  const usable = info.registered && info.lifecycle === "active";
  const paired = info.paired_device;
  const platform = [paired?.platform, paired?.device_model]
    .filter(Boolean)
    .join(" · ");
  return (
    <div className="detail">
      <Attention
        info={info}
        accounts={accounts}
        chain={chain}
        ops={ops}
        detail
      />
      <LinkPanel info={info} chain={chain} endpoints={endpoints} ops={ops} />
      <section className="detail-section" aria-labelledby="detail-device">
        <div className="detail-head">
          <h2 id="detail-device">{t("Device")}</h2>
          <p className="hint">
            {t(
              "Each controller pairs with exactly one device: a phone, tablet or computer with ChatGPT. To control another computer from the same device, create another controller.",
            )}
          </p>
        </div>
        <dl className="facts">
          <div>
            <dt>{t("Name")}</dt>
            <dd>{deviceName(info)}</dd>
          </div>
          {paired && platform && (
            <div>
              <dt>{t("Platform")}</dt>
              <dd>{platform}</dd>
            </div>
          )}
          {paired && (
            <div>
              <dt>{t("Last connected")}</dt>
              <dd>
                {paired.last_seen_at
                  ? new Date(paired.last_seen_at * 1000).toLocaleString(
                      locale(),
                    )
                  : t("Authorized device")}
              </dd>
            </div>
          )}
          {paired && (
            <div>
              <dt>{t("Device ID")}</dt>
              <dd className="id-text fact-quiet">{paired.client_id}</dd>
            </div>
          )}
        </dl>
        {usable && paired && (
          <div className="actions">
            <button
              onClick={() => pairController(ops, info, t("Pair device again"))}
            >
              {t("Pair device again")}
            </button>
          </div>
        )}
      </section>
      <section className="detail-section" aria-labelledby="detail-identity">
        <div className="detail-head">
          <h2 id="detail-identity">{t("Details")}</h2>
          <p className="hint">
            {t(
              "The fixed name restores this controller's identity if it's deleted and created again with the same account.",
            )}
          </p>
        </div>
        <Identity info={info} accounts={accounts} />
        {info.lifecycle !== "deleting" && (
          <div className="actions">
            <button
              className="danger-text"
              onClick={() => open(ops, { kind: "delete", device: id })}
            >
              {t("Delete controller")}
            </button>
          </div>
        )}
      </section>
    </div>
  );
}

export function DeviceDetail({
  id,
  accounts,
  endpoints,
  chains,
  ops,
}: {
  id: string;
  accounts: AccountInfo[];
  endpoints: EndpointInfo[];
  chains: Chain[];
  ops: Ops;
}) {
  const info = endpoints.find(
    (item) => item.endpoint.device_id === id && item.endpoint.kind === "out",
  );
  useEffect(() => {
    if (info) syncDevices(ops.api, [info]);
  }, [id]);
  if (!info) return <Missing ops={ops} controller={false} />;
  const chain = chains.find((c) => c.binding.out_id === id);
  const presence =
    (chain && presenceText(routeState(chain))) || storedPresence(info);
  const usable = info.registered && info.lifecycle === "active";
  return (
    <div className="detail">
      <Attention
        info={info}
        accounts={accounts}
        chain={chain}
        ops={ops}
        detail
      />
      <LinkPanel info={info} chain={chain} endpoints={endpoints} ops={ops} />
      <section className="detail-section" aria-labelledby="detail-computer">
        <div className="detail-head">
          <h2 id="detail-computer">{t("Computer")}</h2>
          <p className="hint">
            {t(
              "Each controlled device drives exactly one computer. If the computer was reinstalled or replaced, pair it again.",
            )}
          </p>
        </div>
        <dl className="facts">
          <div>
            <dt>{t("Name")}</dt>
            <dd>
              {computerName(info)}
              {presence && <span className="fact-note">{presence}</span>}
            </dd>
          </div>
          {info.target && info.target_last_seen_at != null && (
            <div>
              <dt>{t("Last online")}</dt>
              <dd>
                {new Date(info.target_last_seen_at * 1000).toLocaleString(
                  locale(),
                )}
              </dd>
            </div>
          )}
          {info.target && (
            <div>
              <dt>{t("Computer ID")}</dt>
              <dd className="id-text fact-quiet">
                {info.target.environment_id}
              </dd>
            </div>
          )}
        </dl>
        {usable && info.target && (
          <div className="actions">
            <button onClick={() => open(ops, { kind: "pair-out", device: id })}>
              {t("Pair computer again")}
            </button>
          </div>
        )}
      </section>
      <section className="detail-section" aria-labelledby="detail-identity">
        <div className="detail-head">
          <h2 id="detail-identity">{t("Details")}</h2>
          <p className="hint">
            {t(
              "The fixed name restores this device's identity if it's deleted and created again with the same account.",
            )}
          </p>
        </div>
        <Identity info={info} accounts={accounts} />
        <div className="actions">
          {info.lifecycle !== "deleting" && (
            <button
              className="danger-text"
              onClick={() => open(ops, { kind: "delete", device: id })}
            >
              {t("Delete controlled device")}
            </button>
          )}
        </div>
      </section>
    </div>
  );
}
