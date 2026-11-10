# Política de Segurança

## Versões suportadas

| Versão | Suporte |
|---|---|
| Última release (0.x) | ✅ Recebe correções de segurança |
| Versões anteriores a 0.x | ❌ Sem suporte |

## Reportando vulnerabilidades

**Não abra issue pública** para vulnerabilidades de segurança.

Reporte de forma privada via [GitHub Security Advisories](https://github.com/joao-pedro-gms/arandu/security/advisories/new)
ou por e-mail para **joaopedroguimaraes@protonmail.com** (chave PGP disponível
se solicitada).

## O que esperar

- Confirmação de recebimento em até **3 dias úteis**
- Análise inicial e severidade (CVSS) em até **7 dias úteis**
- Correção priorizada conforme severidade:
  - **Crítica/Alta:** patch em até 30 dias
  - **Média:** próxima release planejada
  - **Baixa:** backlog público

## Escopo

O Arandu é um gerenciador de arquivos local: não executa código remoto, não tem
servidor embutido e não coleta telemetria. Vulnerabilidades típicas de interesse:

- Exposição indevida de dados do usuário (leitura/escrita fora do permitido)
- Falhas no sandbox de plugins (quando a fase 3 existir)
- Dependências com CVEs (auditadas via cargo-deny)
- Crash reports: opt-in, salvos **apenas localmente** — nunca enviados
