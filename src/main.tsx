import ReactDOM from "react-dom/client";
import App from "./App";
import "./index.css";

// 刻意不套 <React.StrictMode>：StrictMode 在 dev 下会把 effect 挂载两次，
// 而创建会话不是幂等的 —— 会真的建出两个会话，其中一个被泄漏成孤儿后台任务 +
// 磁盘文件。这里的双调用代价大于它带来的检查收益。
ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(<App />);
