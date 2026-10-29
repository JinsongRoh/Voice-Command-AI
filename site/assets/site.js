/* Voice Command AI landing page: language switch (EN | KO) and copy button.
   English lives in the HTML; Korean strings live here. No tracking, no cookies.
   localStorage only remembers the language choice. */
(function () {
  "use strict";

  var STORAGE_KEY = "vca-lang";
  var REPORT = "https://github.com/JinsongRoh/Voice-Command-AI/blob/dev/m0-foundation/docs/m0-report.md";

  var KO = {
    "meta.title": "Voice Command AI · 말로 지휘하고, 흐름을 관리하다.",
    "meta.desc": "내 PC의 AI 코딩 에이전트를 휴대폰에서 음성으로 확인하고 지시하는 오픈소스 도구입니다. Claude Code, Codex, Gemini CLI 등을 지원하며 종단 간 암호화와 MIT 라이선스를 적용했습니다. 현재 프리릴리스입니다.",
    "meta.ogTitle": "Voice Command AI: 말로 지휘하고, 흐름을 관리하다.",
    "meta.ogDesc": "내 PC의 모든 AI 코딩 에이전트를 휴대폰에서 음성으로 지휘하세요. 오픈소스, 프리릴리스.",
    "meta.locale": "ko_KR",
    "meta.localeAlt": "en_US",

    "skip": "본문으로 건너뛰기",
    "brand.home": "Voice Command AI 홈",
    "nav.label": "주요 메뉴",
    "nav.why": "왜 VCA인가",
    "nav.features": "기능",
    "nav.how": "작동 방식",
    "nav.measured": "검증 결과",
    "nav.oss": "오픈소스",
    "lang.label": "언어",
    "gh.aria": "GitHub의 Voice Command AI",

    "hero.symbolAlt": "Voice Command AI 심볼",
    "hero.badge": "프리릴리스 · 2026년 10월 M0 완료",
    "hero.title": "<span class=\"line\">말로 지휘하고,</span> <span class=\"line\">흐름을 관리하다.</span>",
    "hero.lead": "내 PC의 모든 AI 코딩 에이전트를, 휴대폰에서 목소리로 지휘하세요.",
    "hero.sub": "PC에서 도는 가벼운 서버가 모든 에이전트 세션을 맡습니다. 휴대폰에는 세션이 카드로 보이고, 명령은 창 포커스를 빼앗지 않고 정확한 세션에 도착합니다.",
    "cta.github": "GitHub에서 보기",
    "cta.guide": "사용자 가이드 (영문)",
    "hero.meta1": "MIT 라이선스",
    "hero.meta2": "종단 간 암호화",
    "hero.meta3": "내 PC에서 실행, VCA 클라우드 없음",

    "g.alt": "VCA 앱 예시 화면: 세션 카드 세 장과 api-server 세션으로 보낼 음성 명령",
    "g.conn": "연결됨 · 암호화",
    "g.run": "실행 중",
    "g.wait": "입력 필요",
    "g.done": "완료",
    "g.msg1": "테스트 스위트 실행 중…",
    "g.msg2": "데이터베이스 마이그레이션을 적용할까요?",
    "g.msg3": "테스트 완료. 실패한 2개 항목을 확인하세요.",
    "g.to": "받는 세션",
    "g.text": "통합 테스트 돌리고 실패한 항목만 요약해 줘.",
    "g.send": "보내기",
    "g.caption": "예시 화면입니다. 보내기 전에 어느 세션으로 갈지 먼저 보여 줍니다.",

    "why.eyebrow": "왜 VCA인가",
    "why.title": "에이전트는 여럿, 키보드는 하나.",
    "why.intro": "에이전트 세션을 여러 개 띄우면 서브 에이전트, 컴파일러, 테스트 러너, 보조 창이 한 대의 PC로 몰립니다. 가장 먼저 무너지는 건 이 세 가지입니다.",
    "why.with": "VCA는 이렇게",
    "why.p1.title": "포커스 탈취",
    "why.p1.body": "새 창이 계속 포커스를 가져가서 프롬프트와 키 입력이 엉뚱한 터미널로 들어갑니다.",
    "why.p1.fix": "가상 터미널을 통해 대상 세션에 명령을 직접 전달합니다. 키 입력을 흉내 내지 않습니다.",
    "why.p2.title": "자원 고갈",
    "why.p2.body": "에이전트들이 서로를 모른 채 빌드와 테스트를 시작하고, 결국 CPU와 메모리가 바닥나 PC가 멈춥니다.",
    "why.p2.fix": "리소스 브로커가 무거운 작업을 정해진 슬롯 수만큼만 실행하고 나머지는 대기열에 둡니다.",
    "why.p3.title": "탐색 부담",
    "why.p3.body": "기존 원격 제어 앱은 명령 하나를 보낼 때마다 프로젝트에 들어갔다 나오기를 반복하게 합니다.",
    "why.p3.fix": "모든 세션이 카드 한 벌에 모입니다. 고르고, 말하고, 보내면 끝입니다.",

    "feat.eyebrow": "기능",
    "feat.title": "지금 프리릴리스에서 동작하는 기능",
    "feat.intro": "소스에서 빌드해 처음부터 끝까지 동작합니다. ‘출시 예정’ 표시는 아직 사용할 수 없는 기능입니다.",
    "soon": "출시 예정",
    "f1.t": "세션 카드",
    "f1.d": "PC의 모든 에이전트 세션을 실시간 카드로 보여 줍니다. 프로젝트, 에이전트, 상태, 알림을 한눈에 확인하세요.",
    "f2.t": "음성 우선",
    "f2.d": "탭하고 말하면 받아쓰기가 말하는 대로 실시간으로 나타납니다. 내용을 확인하고 보내세요. 개발자 용어 엔진이 기술 용어를 정확하게 지켜 줍니다.",
    "f3.t": "포커스가 필요 없음",
    "f3.d": "명령은 가상 터미널이나 에이전트 고유 채널로 대상 세션에 바로 전달됩니다. 키 입력 시뮬레이션은 쓰지 않습니다.",
    "f4.t": "실시간 보기",
    "f4.d": "어떤 세션이든 읽기 쉬운 채팅 화면이나 원본 터미널로 따라가며, 그 자리에서 대화를 이어 갈 수 있습니다.",
    "f5.t": "알림",
    "f5.d": "세션이 끝나거나, 입력이 필요하거나, 결정을 요청하면 바로 알려 줍니다. 탭 한 번으로 답하세요.",
    "f6.t": "리소스 브로커",
    "f6.d": "세션별 CPU, 메모리, 디스크, 네트워크를 지켜봅니다. 무거운 작업은 정책에 따라 실행, 대기, 제한, 일시 정지됩니다.",
    "f7.t": "여러 대의 PC",
    "f7.d": "집, 사무실, 서버 PC를 앱 하나에 페어링하세요.",
    "f8.t": "페어링과 암호화",
    "f8.d": "일회용 페어링 링크, TOTP 2단계 인증, 단기 토큰, Noise 종단 간 암호화, 감사 로그, 킬 스위치를 갖췄습니다.",
    "f9.t": "연결 방식은 직접 선택",
    "f9.d": "LAN 직접 연결, Tailscale, Cloudflare Tunnel, 또는 직접 구성한 방식. 연결 방식을 정하기 전까지 서버는 로컬호스트에서만 대기합니다.",
    "f10.t": "음성 인식 엔진 선택",
    "f10.d": "직접 운영하는 Whisper 서버 같은 OpenAI 호환 음성 서버, 또는 OpenAI와 Google Cloud를 연결할 수 있습니다.",
    "f11.t": "멀티 에이전트 오케스트레이션",
    "f11.d": "선택형 오케스트레이터 세션을 두고 여러 세션이 하나의 작업을 함께 처리합니다.",
    "f12.t": "DNS Pilot 연결",
    "f12.d": "DNS Pilot 터널 또는 P2P로 내 PC에 접속합니다. 여러 연결 방식 중 하나로, 선택 사항입니다.",

    "how.eyebrow": "작동 방식",
    "how.title": "휴대폰은 오직 내 PC와만 통신합니다.",
    "how.intro": "한 번 페어링하면 앱은 Noise 종단 간 암호화 채널로 PC의 VCA 서버에 연결됩니다. 에이전트 세션은 서버가 직접 관리하며, 어떤 데이터도 VCA 클라우드를 거치지 않습니다.",
    "d.alt": "구성도: 휴대폰이나 데스크톱의 VCA 앱이 종단 간 암호화 채널로 내 PC의 VCA 서버에 연결되고, 서버는 Claude Code, Codex, Gemini CLI, Antigravity, Grok 같은 에이전트를 관리하며 선택적으로 음성 서버를 사용합니다.",
    "d.app": "VCA 앱",
    "d.appSub": "iPhone · 데스크톱",
    "d.app1": "세션 카드",
    "d.app2": "음성 받아쓰기",
    "d.app3": "실시간 보기와 알림",
    "d.e2e": "종단 간 암호화",
    "d.noise": "Noise 프로토콜",
    "d.via1": "LAN · Tailscale",
    "d.via2": "Cloudflare · 사용자 지정",
    "d.server": "VCA 서버",
    "d.serverSub": "내 PC에서 실행",
    "d.s1": "세션 관리자 (PTY)",
    "d.s2": "에이전트 어댑터",
    "d.s3": "리소스 브로커",
    "d.s4": "음성 파이프라인",
    "d.s5": "인증 · 감사 로그 · API",
    "d.agents": "내 PC의 에이전트",
    "d.more": "그 외",
    "d.stt": "음성 서버 (선택)",
    "d.sttSub": "OpenAI 호환 서비스",
    "d.caption": "파란 테두리는 전용 어댑터가 있는 에이전트입니다. 나머지는 범용 어댑터로 동작합니다.",
    "s1.t": "VCA로 에이전트 실행",
    "s1.d": "관리 세션은 창 포커스와 상관없이 앱에서 입력을 받습니다.",
    "s2.t": "한 번만 페어링",
    "s2.d": "일회용 페어링 링크를 휴대폰에서 열고, 인증 앱의 6자리 코드를 입력합니다.",
    "s3.t": "카드를 고르고 말하기",
    "s3.d": "세션을 고르고 말한 뒤, 대상과 내용을 확인하고 보냅니다.",

    "m.eyebrow": "M0 검증 결과",
    "m.title": "실제 장비에서 측정한 숫자입니다.",
    "m.intro": "M0 마일스톤에서는 2026년 10월, Windows, macOS, Linux, iPhone에서 핵심 구성 요소를 하나씩 검증했습니다. 주요 결과는 다음과 같습니다.",
    "m.s1": "LAN과 Tailscale에서 각각 2시간 연속 테스트한 요청 응답 수. 유실 0건",
    "m.v2": "1.3<small>초</small>",
    "m.s2": "서버 재시작 후 다시 연결되기까지 걸린 시간. 재페어링 필요 없음",
    "m.v3": "~0.3<small>초</small>",
    "m.s3": "약 10초 분량의 음성을 GPU Whisper 서버에서 받아쓰는 데 걸린 시간",
    "m.v4": "5.5<small>ms</small>",
    "m.s4": "암호화 채널을 거친 LAN 왕복 지연 시간의 중앙값",
    "m.c1": "Windows·macOS에서 관리 세션 검증",
    "m.c2": "실제 Claude Code 훅과 Codex 입력 검증",
    "m.c3": "실제 cargo 빌드로 실행 승인 제어 검증",
    "m.c4": "iPhone 음성 입력부터 받아쓰기까지 검증",
    "m.note": "M0에서 아직 검증하지 않은 항목: Cloudflare Tunnel과 DNS Pilot 연결, Android 빌드, 클라우드 에이전트 API. <a href=\"" + REPORT + "\">M0 보고서 읽기 (영문)</a>",

    "p.eyebrow": "지원 범위",
    "p.title": "쓰던 에이전트를 그대로 쓰세요.",
    "p.agentsTitle": "에이전트",
    "p.agentsBody": "터미널에서 도는 에이전트라면 관리 세션으로 실행할 수 있습니다. Claude Code와 Codex는 전용 어댑터를 지원하고, 나머지는 출력에서 프롬프트와 대기 상태를 읽어 내는 범용 어댑터로 동작합니다.",
    "p.more": "그 외",
    "p.legend": "전용 어댑터",
    "p.tm": "제품명은 각 소유자의 상표입니다.",
    "p.platTitle": "플랫폼",
    "p.platBody": "서버는 개발 PC에서, 앱은 데스크톱과 휴대폰에서 실행됩니다.",
    "p.kindDesk": "서버와 데스크톱 앱",
    "p.kindApp": "모바일 앱",
    "p.src": "소스 빌드",
    "p.internal": "내부 테스트 빌드",

    "t.eyebrow": "직접 써 보기",
    "t.title": "설치 파일은 아직 없습니다. 명령 몇 줄이면 빌드됩니다.",
    "t.body": "소스에서 서버와 CLI를 설치하고, 서버를 띄운 다음, 첫 에이전트를 VCA로 실행하세요.",
    "t.reqs": "Git과 Rust 툴체인이 필요합니다. Windows에서는 ‘C++를 사용한 데스크톱 개발’ 빌드 도구도 설치하세요.",
    "t.copy": "복사",
    "t.copied": "복사했습니다",
    "t.codeLabel": "빠른 시작 명령어",

    "o.eyebrow": "오픈소스",
    "o.title": "MIT 라이선스. 모든 기능을 무료로.",
    "o.intro": "코어는 Rust로, 앱은 Tauri 2로 만들었습니다. 어떤 기능도 비용을 내지 않고 쓸 수 있습니다.",
    "o.c1.t": "코드 살펴보기",
    "o.c1.d": "서버, CLI, 공용 UI, 데스크톱·모바일 앱이 MIT 라이선스로 한 저장소에 있습니다.",
    "o.c1.l": "저장소 둘러보기",
    "o.c2.t": "기여하기",
    "o.c2.d": "코드, 에이전트 어댑터, 번역, 내 언어의 개발자 용어 사전까지. 이슈와 풀 리퀘스트를 환영합니다.",
    "o.c2.l": "이슈 열기",
    "o.c3.t": "프로젝트 후원",
    "o.c3.d": "DNS Pilot은 VCA를 만드는 팀이 개발하며, 구독료는 VCA 개발에 쓰입니다. 연동이 출시되면 DNS Pilot을 연결 방식으로 고르는 것도 후원이 됩니다. 어디까지나 선택 사항이며, Tailscale, Cloudflare Tunnel, LAN, 사용자 지정 방식도 똑같이 지원합니다.",
    "o.c3.l": "저장소에 별 달기",

    "footer.label": "푸터 메뉴",
    "footer.slogan": "말로 지휘하고, 흐름을 관리하다.",
    "footer.guide": "사용자 가이드",
    "footer.report": "M0 보고서",
    "footer.brand": "브랜드 가이드라인",
    "footer.license": "MIT 라이선스",
    "footer.privacy": "추적, 분석 도구, 쿠키를 사용하지 않습니다."
  };

  var EN_EXTRA = { "t.copied": "Copied" };
  var EN = {};
  var current = "en";

  var textNodes = document.querySelectorAll("[data-i18n]");
  var attrNodes = document.querySelectorAll("[data-i18n-attr]");

  // Capture the English source from the markup once.
  textNodes.forEach(function (el) {
    var key = el.getAttribute("data-i18n");
    if (!(key in EN)) EN[key] = el.innerHTML;
  });
  attrNodes.forEach(function (el) {
    parseAttr(el).forEach(function (pair) {
      if (!(pair.key in EN)) EN[pair.key] = el.getAttribute(pair.attr);
    });
  });
  Object.keys(EN_EXTRA).forEach(function (k) { EN[k] = EN_EXTRA[k]; });

  function parseAttr(el) {
    return el.getAttribute("data-i18n-attr").split(";").map(function (part) {
      var i = part.indexOf(":");
      return { attr: part.slice(0, i).trim(), key: part.slice(i + 1).trim() };
    });
  }

  function t(key) {
    var dict = current === "ko" ? KO : EN;
    return key in dict ? dict[key] : EN[key];
  }

  function apply(lang) {
    current = lang === "ko" ? "ko" : "en";
    document.documentElement.lang = current;

    textNodes.forEach(function (el) {
      var value = t(el.getAttribute("data-i18n"));
      if (value == null) return;
      // SVG text and <title> take plain text; HTML elements may carry inline markup from our own dictionary.
      if (el instanceof SVGElement || el.tagName === "TITLE") el.textContent = value;
      else el.innerHTML = value;
    });
    attrNodes.forEach(function (el) {
      parseAttr(el).forEach(function (pair) {
        var value = t(pair.key);
        if (value != null) el.setAttribute(pair.attr, value);
      });
    });

    document.querySelectorAll(".lang-toggle button").forEach(function (b) {
      b.setAttribute("aria-pressed", String(b.getAttribute("data-lang") === current));
    });

    var canonical = document.querySelector('link[rel="canonical"]');
    if (canonical) canonical.href = current === "ko" ? "https://vca.metavu.io/?lang=ko" : "https://vca.metavu.io/";
  }

  function save(lang) {
    try { localStorage.setItem(STORAGE_KEY, lang); } catch (e) { /* storage may be unavailable */ }
  }

  function syncUrl(lang) {
    try {
      var url = new URL(location.href);
      if (lang === "ko") url.searchParams.set("lang", "ko");
      else url.searchParams.delete("lang");
      history.replaceState(null, "", url.pathname + url.search + url.hash);
    } catch (e) { /* ignore */ }
  }

  // The head script already resolved the language (URL > saved choice > browser).
  apply(document.documentElement.lang);

  document.querySelectorAll(".lang-toggle button").forEach(function (b) {
    b.addEventListener("click", function () {
      var lang = b.getAttribute("data-lang");
      if (lang === current) return;
      apply(lang);
      save(lang);
      syncUrl(lang);
    });
  });

  // Copy button for the quick start block.
  var status = document.getElementById("copy-status");
  document.querySelectorAll(".copy-btn").forEach(function (btn) {
    if (!navigator.clipboard) { btn.hidden = true; return; }
    btn.addEventListener("click", function () {
      var src = document.querySelector(btn.getAttribute("data-copy"));
      if (!src) return;
      // Strip the "$ " prompts so the commands paste cleanly.
      var text = src.textContent.split("\n").map(function (line) { return line.replace(/^\$ /, ""); }).join("\n");
      navigator.clipboard.writeText(text).then(function () {
        btn.textContent = t("t.copied");
        if (status) status.textContent = t("t.copied");
        setTimeout(function () {
          btn.textContent = t("t.copy");
          if (status) status.textContent = "";
        }, 1800);
      }, function () { /* clipboard denied: leave the button as is */ });
    });
  });
})();
