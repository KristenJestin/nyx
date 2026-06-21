# PDR-0007 - Sidecar Rust et frontières IPC

**Status: validated**

## Context

La migration Electron actuelle utilise deux relais : IPC Electron entre renderer et main, puis IPC Node vers un `core-host` chargé de N-API. Ce host possède déjà de la logique runtime significative : PTY, commandes, lifecycle, busy-state, reprise et routage MCP. Il crée donc une seconde autorité en TypeScript et multiplie les ressources longues durées à fermer.

Charger N-API directement dans Electron main réduirait un processus mais ferait partager au shell graphique les risques de crash natif, de blocage et de fork PTY déjà observés. Utiliser HTTP/WebSocket localhost comme transport interne ajouterait ports, authentification et surface d'attaque sans bénéfice produit.

## Decision

Le backend est livré comme un **exécutable sidecar Rust**, enfant direct et supervisé d'Electron main. Ce n'est pas un daemon et il ne survit pas à l'application.

Le renderer utilise uniquement l'IPC Electron allowlisté via preload. Electron main utilise des pipes locaux `stdin/stdout` vers le sidecar ; `stderr` transporte uniquement ses logs. Le protocole V1 utilise des frames MessagePack préfixées par leur longueur, est versionné et corrélé, et supporte directement les payloads binaires. Les sorties PTY utilisent des frames binaires bornées et de la backpressure, pas du JSON/base64 par chunk.

Chaque package embarque un sidecar compilé pour sa cible : `.exe` sous Windows et binaire natif sous Linux/macOS, pour x64 ou arm64 selon le package. Le sidecar est placé hors `app.asar`, signé avec l'application lorsque la plateforme l'exige, et lancé depuis les resources Electron.

N-API et le `core-host` Node sont des mécanismes transitoires de migration à supprimer après obtention de la parité fonctionnelle du sidecar.

## Consequences

- un seul backend métier et runtime, en Rust ;
- Electron main reste un superviseur et adaptateur desktop mince ;
- un crash backend n'emporte pas automatiquement Chromium et l'UI ;
- disparition des contraintes ABI N-API liées à la version Node embarquée par Electron ;
- packaging d'un binaire Rust par OS/architecture ;
- obligation d'un protocole et de tests contractuels Rust/TypeScript ;
- obligation de gérer explicitement shutdown, crash, backpressure et arbre de processus sur chaque OS ;
- le serveur MCP HTTP reste dans Rust mais ne remplace pas le transport interne par pipes.
